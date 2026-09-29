//! The application entity: the connection, auth, the session list and the shell.
//!
//! # Thread model
//!
//! One provider per process, owned here behind an [`Arc`](std::sync::Arc)
//! as [`SharedProvider`](crate::wire::SharedProvider), sharing its one
//! spawned child with the legacy [`MuseClient`] transport session views
//! still ride (see [`crate::conn::Legacy`]). The child already runs its own
//! reader and writer threads, so the pipe never touches the UI thread.
//! Two directions cross the boundary:
//!
//! * **Events in.** [`crate::conn::connect`] hands back two `futures`
//!   receivers, each fed by one bridging thread: provider events and the
//!   raw transport events. A single foreground task drains the raw stream
//!   and calls [`SessionView::apply`], so folding happens on the UI thread
//!   in wire order and a frame always renders a consistent transcript; a
//!   second task observes the provider stream.
//! * **Commands out.** Every request blocks, so every one of them runs on
//!   `background_spawn` and returns through `update`. The UI thread issues
//!   intents and never waits, and [`crate::wire::WireCall`] is the one shape
//!   every one of those calls has — [`crate::wire::ProviderCall`] for the
//!   ones already speaking [`Command`](provider::Command).
//!
//! # Screens
//!
//! Two, and the auth probe decides which: the login screen (spec §3.2) or
//! the shell. The shell's right
//! pane opens on demand (⌘⌥B); the column starts closed.
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
//! * [`crate::resize`] — the sidebar and right-pane dividers' drags.
//! * [`crate::steps`] — `--steps` and `--login-steps`: the verb tables, the
//!   parser and the two runners.
//! * [`crate::wire`] — background call, then update.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use aui::composer::composer_state_rows;
use aui::data::{button, icon_button, ButtonSize};
use aui::util::{interaction, TrackInteraction};
use aui::feedback::{banner, BannerKind, BannerRun};
use aui::keys::{Cancel, FocusNext, FocusPrev, TogglePalette, ToggleSidebar};
use aui::overlay::DialogKind;
use aui::shell::{
    RESIZE_HANDLE_W, app_shell, clamp_sidebar_width, drag_capture_overlay,
    header_cell, resize_handle, sidebar_header,
};
use aui::workbench::{terminal_dock, terminal_tabs, TermTab, TerminalDockAction, TerminalTabsAction};
use aui_icons::{icon, IconName, Provider};
use aui_motion::{SpringKind, spring_px};
use aui_terminal::{terminal_grid, TerminalGridIntent};
use aui_tokens::{scale, ActiveAui, AuiStyled, AuiTheme};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui::{
    Bounds, Pixels, PlatformInput, ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, actions, div, point, prelude::*, px, AnyElement, App, Context, Entity, ExternalPaths, FocusHandle,
    Focusable, ListState, SharedString, Subscription, Task, Window,
};
use gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Root;
use muse_client::schema::{
    AccountStateKind, SessionListParams, SessionResumeParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};
use provider::ProviderEvent;

use crate::auth::Identity;
use crate::conn::{self, Severity};
use crate::index::{self, IndexEntry};
use crate::login::{Auth, Login};
use crate::overlays::{Dialog, DialogAction, MenuKind, Overlays, Palette, PaletteKind};
use crate::providers::ProviderId;
use crate::resize::{ResizeDrag, RightResizeDrag};
use crate::right;
use crate::shot::CaptureToken;
use crate::session::{self, Draft, SessionEvent, SessionHost, SessionView};
use crate::tier::Tier;
use crate::sessions::{self, SessionMeta};
use crate::projects::{self, Project, Projects};
use crate::app::list::ListCache;
use aui::nav::{sidebar_list_state, SidebarRow};

use crate::sidebar::{self, Grouping, SessionEntry};
use crate::sidebar_view::{SidebarKey, SidebarPane, SidebarRegroupKey, SidebarWheelState};
use crate::search::{FileHit, SessionHit};
use crate::terminal::{self, TabOwner, TerminalHost, TerminalService};
use crate::wire::{SharedProvider, WireCall};
use crate::{layout, Args};

actions!(
    baaz,
    [
        /// Send the composer's draft (Enter).
        SendTurn,
        /// Interject into the running turn (⌘↩).
        SteerTurn,
        /// Stop the running turn and retract its prompt (⌃C).
        Interrupt,
        /// Start a new session in this workspace (⌘N).
        NewSession,
        /// Open the Projects palette (⌘⇧O).
        AddProject,
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
        /// Toggle the terminal dock (⌃`).
        ToggleTerminal,
        /// Toggle the right pane (⌘⌥B).
        ToggleRightPane,
        /// Open a new terminal tab on the current project.
        NewTerminal,
        /// Send SIGINT to the active terminal tab (⌃C while the dock is focused).
        TerminalSigint,
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
        /// Open the Settings dialog (⌘,).
        OpenSettings,
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
        /// Move the Skills page selection up (↑).
        SkillsUp,
        /// Move the Skills page selection down (↓).
        SkillsDown,
        /// Flip the selected skill on or off (Space).
        SkillsToggle,
        /// Focus the Skills page detail pane (Enter).
        SkillsEnter,
        /// Focus the Skills page search field (⌘F).
        SkillsFind,
        /// Leave the Skills page (Escape).
        SkillsClose,
    ]
);

/// The key context the sidebar's inline rename field wears, so Enter commits
/// the name instead of reaching the composer's send.
pub(crate) const RENAME_CONTEXT: &str = "BaazRename";

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
pub(crate) const COMPOSER_CONTEXT: &str = "BaazComposer";

/// The predicate the palette's arrow keys bind in: a menu scrim above a text
/// field. The palette scrim wears [`aui::keys::MENU_CONTEXT`] and the query
/// editor wears the input layer's `Input` context, whose own `up`/`down`
/// bindings (caret moves) match deeper than anything on an ancestor — which
/// is why the scrim's `SelectPrev`/`SelectNext` never fired while a query
/// field was focused. A descendant predicate matches at the full depth of
/// the focused field, tying the textarea's own binding; the tie breaks by
/// registration order, and Baaz binds after the libraries (`aui::init`
/// before [`bind_keys`] in `main.rs`), so the palette's binding wins. Only
/// the two arrows are rebound, and only under a menu scrim, so typing —
/// `j`, `k`, every other key — still reaches the field untouched.
pub(crate) const PALETTE_QUERY_CONTEXT: &str = "AuiMenu > Input";

/// The context the terminal dock wears (`docs/14-terminal.md:192`). The grid
/// runs under it: every key reaches the pty except ⌃`, ⌘K, ⌘B, ⌘W, ⌘Q, ⌘N
/// (bound above the grid, at the root) and ⌘C with a selection (which the
/// grid copies itself). The `NoAction` bindings in [`bind_keys`] keep the
/// composer's Enter, paste and history keys — and the turn's ⌃C — from
/// firing while the dock holds the keyboard: `BaazTerminal` hangs deeper in
/// the tree than they do, so it wins, and `NoAction` consumes the keystroke
/// without an action, which hands it to the grid's own key handler.
pub(crate) const TERMINAL_CONTEXT: &str = "BaazTerminal";

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

/// How far above dead centre a hero column sits, in points.
///
/// A column centred exactly in the pane reads low: the eye puts the optical
/// centre above the geometric one, and the sidebar and composer give the
/// window weight at the bottom already. Applied as bottom padding, so the
/// column still centres — just in a slightly shorter box.
pub(crate) const HERO_LIFT: f32 = 48.0;
pub(crate) const PALETTE_SCRIM: f32 = 0.4;
/// How many rows the palette lists. The sidebar's search is the way through a
/// longer list; this is the way back to something recent.
pub(crate) const PALETTE_ROWS: usize = 12;
/// How many sessions one refresh will spend a `session/read` on. A workspace
/// with two hundred untitled sessions should not open two hundred reads on the
/// first frame; the rest are picked up by the next refresh.
const MAX_TITLE_READS: usize = 12;

/// Binds Baaz's own keys on top of the library's.
///
/// `aui::init` has already bound ⌘B, ⌘K, ⌘\\, Escape, Tab and the approval
/// triad; these are the ones only this app knows about. The window keys live
/// here too, so the native menu bar ([`set_menus`]) can show their shortcuts:
/// macOS reads each item's shortcut from the keymap.
///
/// The bindings themselves live in one table, [`crate::keymap::KEYMAP`];
/// this installs what [`crate::keymap::load`] returns: the table first, the
/// person's `keymap.json` second, so a binding written in the file wins by
/// gpui's existing depth-then-order rule.
///
/// The load is one best-effort file read, done once here at startup before
/// any window opens: the event loop is not yet dispatching input or painting
/// frames, so the blocking read cannot stall the UI. A refused user entry is
/// never silent — every warning [`crate::keymap::load`] collects goes
/// through `baaz_log!`, the app's diagnostics surface.
pub fn bind_keys(cx: &mut App) {
    let loaded = crate::keymap::load();
    for warning in &loaded.warnings {
        crate::baaz_log!("{warning}");
    }
    cx.bind_keys(loaded.bindings);
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
        crate::baaz_log!("CloseWindow");
        crate::tier::cleanup_probes();
        // Same as the red dot: hide the app rather than remove the window,
        // so the session and the `muse serve` child survive and the Dock
        // icon or Cmd-Tab bring the same window back (`main.rs`,
        // `open_shell_window`). Deferred: menu dispatch already holds this
        // window in an update.
        cx.defer(|cx| cx.hide());
    });
    cx.on_action(|_: &QuitApp, cx: &mut App| {
        crate::baaz_log!("QuitApp (global)");
        // A running terminal command asks first (D52), naming the command:
        // the first window holding one opens the confirm dialog instead of
        // quitting. This stays a global listener — validation consults the
        // focused window's dispatch tree, which is empty on the login
        // screen — and reaches the window's `Harness` through its root.
        for window in cx.windows() {
            let asked = window
                .downcast::<Root>()
                .and_then(|handle| {
                    handle
                        .update(cx, |root, _, cx| {
                            root.view()
                                .clone()
                                .downcast::<Harness>()
                                .map(|harness| harness.update(cx, |this, cx| this.maybe_confirm_quit(cx)))
                                .unwrap_or(false)
                        })
                        .ok()
                })
                .unwrap_or(false);
            if asked {
                return;
            }
        }
        // Quitting for real: hang up every provider lane's child before the
        // probes go and the process exits, so no `claude` or `codex` child
        // outlives the app. Only after the confirm above — a cancelled quit
        // must not kill live sessions.
        for window in cx.windows() {
            let _ = window
                .downcast::<Root>()
                .and_then(|handle| {
                    handle
                        .update(cx, |root, _, cx| {
                            root.view()
                                .clone()
                                .downcast::<Harness>()
                                .map(|harness| {
                                    harness.update(cx, |this, cx| this.shutdown_provider_views(cx))
                                })
                                .unwrap_or(())
                        })
                        .ok()
                });
        }
        crate::tier::cleanup_probes();
        crate::browser::cleanup_screenshots();
        cx.quit();
    });
    cx.set_menus([
        gpui::Menu::new("Baaz").items([
            gpui::MenuItem::action("About Baaz", ShowAbout),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Quit Baaz", QuitApp),
        ]),
        gpui::Menu::new("File").items([
            gpui::MenuItem::action("Add Project…", AddProject),
            gpui::MenuItem::action("New Session", NewSession),
            gpui::MenuItem::action("Settings…", OpenSettings),
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
            gpui::MenuItem::action("Baaz Documentation", ShowDocs),
        ]),
    ]);
}

mod find;
pub(crate) mod lifecycle;
mod list;
mod titles;

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
/// how many sessions and files came back (finding `performance-9`), and the
/// scope it was queried in — a narrowed palette names its project.
type SearchStatusKey = (bool, usize, usize, bool, String);

/// How to re-run a failed provider open from the inline failure state (Z4).
#[derive(Clone)]
pub(crate) enum ProviderOpenRetry {
    /// Reopen the stored session: a failed sidebar-click reopen.
    Reopen(Box<crate::provider_sessions::ProviderSessionRecord>),
    /// Start a fresh session on the provider: a failed new-session open.
    OpenNew { project: Option<String>, workspace: String },
}

/// A provider open that failed after the UI already moved (Z4): the window
/// lands on the failed session with this inline instead of a modal dialog
/// over another session's transcript.
#[derive(Clone)]
pub(crate) struct ProviderOpenError {
    /// The session the window sits on: the clicked session for a reopen,
    /// `None` for a fresh open that never earned an id.
    pub session_id: Option<String>,
    /// Whose open failed: names the title, the buttons and the composer.
    pub provider: ProviderId,
    /// The open failure, verbatim.
    pub error: String,
    /// What Retry re-runs.
    pub retry: ProviderOpenRetry,
}

/// The whole application.
pub struct Harness {
    pub(crate) args: Args,
    /// The switcher's pick: which registry entry a new session starts on.
    /// Seeded from the command line, changed only by the picker — never by
    /// a live session, which keeps the lane it was created on.
    pub(crate) new_provider: String,
    /// The legacy transport session views still ride. Same child as the
    /// provider below — never a second spawn. Gone with the last legacy
    /// view.
    pub(crate) client: Option<Arc<MuseClient>>,
    /// The one way to talk to the provider: the adapter behind the
    /// enforced capability gate. Set alongside `client` on connect and
    /// cleared with it on reconnect.
    pub(crate) provider: Option<SharedProvider>,
    /// How a new Claude Code / Codex session connects: the production
    /// factory spawns the real CLI child; tests inject a scripted provider.
    /// Never touched on the UI thread — the open path runs it on the
    /// background executor, where connecting and `OpenSession` may block.
    pub(crate) provider_factory: crate::providers::ProviderFactory,
    pub(crate) wire: Wire,
    pub(crate) auth: Auth,
    pub(crate) login: Login,
    /// Y5: first-run connect screen state. At boot `show_connect` is decided
    /// from stored facts only (the shell renders immediately otherwise, and
    /// Muse's `account/read` never gates the window); afterwards only the
    /// person sets it, from Settings → Providers → "Set up providers…".
    pub(crate) show_connect: bool,
    /// The statuses the connect screen renders: cache/script at boot, kept
    /// fresh from the cache file while shown.
    pub(crate) connect_statuses: Vec<crate::provider_status::ProviderStatus>,
    /// Per-row notes appended under the account line (Codex waiting,
    /// terminal-prefill confirmations).
    pub(crate) connect_notes: HashMap<ProviderId, String>,
    /// The Codex Sign in flow's state (Idle until its row starts it).
    pub(crate) codex_login: crate::connect::CodexLogin,
    /// Cancel for the running Codex login thread, replaced on every start.
    pub(crate) codex_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Today's Muse login flow, presented as a sheet over the connect
    /// screen (or over the shell from the account menu) — never the app's
    /// first screen.
    pub(crate) muse_sheet: bool,
    /// Quiet per-provider banners for cached-Connected providers that later
    /// probe Signed out: shown on that provider's composer, never a gate.
    pub(crate) provider_banners: HashMap<ProviderId, String>,
    /// Providers the cache has called Connected this launch: only these
    /// earn a sign-out banner (a provider never connected gets none).
    pub(crate) seen_connected: HashSet<ProviderId>,
    /// A terminal prefill parked by a row action, run in `on_frame` where
    /// the window lives.
    pub(crate) pending_prefill: Option<String>,
    /// A Codex login start parked by its row action, run in `on_frame`.
    pub(crate) pending_codex_start: bool,
    /// The provider-status cache's mtime when last read, so the connect
    /// screen and banners follow background probes without polling reads.
    pub(crate) connect_cache_mtime: Option<std::time::SystemTime>,
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
    /// The handoff chains over full storage, built once per `list_epoch`
    /// and shared by the head, member and view lookups — never rebuilt per
    /// frame or per row (Y2a3). `list_epoch` is the validity key: every
    /// storage change invalidates the list, which retires the index with
    /// it. Interior mutability because the readers (`visible_sessions`,
    /// `chain_head`, the header) only hold `&self`.
    chain_index: RefCell<Option<(u64, sidebar::ChainIndex)>>,
    /// How many chain indexes this run built: the once-per-change proof
    /// the Y2a3 tests read.
    chain_index_builds: std::cell::Cell<u64>,
    pub(crate) index: HashMap<String, IndexEntry>,
    pub(crate) active: Option<Entity<SessionView>>,
    /// The session the UI is pointed at: the sidebar click's target, set the
    /// moment `resume` runs. The centre swaps synchronously, so this usually
    /// names the active view — but the row highlights and the header label
    /// read it, never the view, so the click is acknowledged on its own frame.
    pub(crate) pending_id: Option<String>,
    /// The pending "ensure visible" row id (`scrollIntoView({ block:
    /// "nearest" })` semantics). Set only by
    /// outside-the-sidebar activations — the palette, New session / `+`,
    /// fork, boot `--session`, the `open:` step — and consumed once, on the
    /// first prepaint after activation, by the selected-row / current-group
    /// intents `render_sidebar` installs: above aligns the row's top, below
    /// its bottom, inside moves nothing. A sidebar click never sets it (the
    /// clicked row is under the cursor, hence visible), and neither does a
    /// list refresh, a regroup, a user scroll or a resize drag.
    pub(crate) reveal: Option<String>,
    /// An id [`Self::reveal`] named that has no row yet, and how many
    /// consecutive `reveal_sidebar_row` attempts it has missed on (owner
    /// round 6, part C4 review #1): a draft before its first send, a fork
    /// before `load_sessions`'s reply lands. Distinct from "nowhere to
    /// go" (a known session whose landing row genuinely cannot be found),
    /// which still gives up on the first miss — this one waits up to
    /// [`crate::sidebar_view::REVEAL_UNKNOWN_FRAMES`] attempts for the row
    /// to be born, since the sites that create it (the `turn/started`
    /// local-row insert, a `load_sessions` reply) already notify and would
    /// otherwise race a reveal armed moments earlier every time.
    pub(crate) reveal_unknown: Option<(String, u32)>,
    /// The sidebar column as its own view: embedded with
    /// gpui's `.cached(size_full)`, a clean pane reuses its retained subtree
    /// instead of rebuilding the column on a transcript notify.
    /// [`Harness::sync_sidebar_pane`] re-arms it from `on_frame` whenever
    /// [`SidebarKey`] changes.
    pub(crate) sidebar_pane: Entity<SidebarPane>,
    /// The [`SidebarKey`] the pane last rendered for. `None` until the first
    /// frame, so the pane renders at least once.
    pub(crate) sidebar_key: Option<SidebarKey>,
    /// Parked session views, most-recently-opened first: an MRU of eight.
    /// Switching away parks the view (its event subscription dropped, its
    /// fold, scroll position and draft kept); reopening shows it at once and
    /// tops it up from its last cursor.
    session_cache: Vec<(String, Entity<SessionView>)>,
    /// One unsent draft session per project, by project id: what ⌘N returns
    /// to while nothing has been sent. A draft has no sidebar row; the entry
    /// leaves the map the moment its first turn is accepted (`turn/started`).
    pub(crate) drafts: HashMap<String, String>,
    /// A draft taken from another project's session, waiting for the picked
    /// project's draft session to exist so it can be moved in. Set by the
    /// project menu's retarget, consumed by `new_session_in`.
    pub(crate) pending_draft: Option<Draft>,
    /// `(session id, root)` for a session started outside any project, held
    /// from `session/start` until the sessions list carries its row.
    ///
    /// A session with no project is the one case where nothing else knows
    /// where it lives: `current_project` is deliberately not set, so
    /// `session_workspace` would fall back to the launch directory for a row
    /// it cannot find yet — which on a bundle opened from Finder is `/`.
    /// The header would name the wrong place and the `@` picker would walk
    /// the whole disk to fill itself.
    pub(crate) starting_root: Option<(String, String)>,
    /// Everything that floats: the modal, the open menu and the toasts. One
    /// entity, shared with the session view, which renders the halves that hang
    /// off the composer's own chips (spec §2.3).
    pub(crate) overlays: Entity<Overlays>,
    pub(crate) sidebar_open: bool,
    /// The sidebar divider's width and whatever drag is in flight over it
    /// (see [`crate::resize`]).
    pub(crate) resize: ResizeDrag,
    /// The right pane divider's width and whatever drag is in flight over
    /// it. Never active while [`Self::resize`] is: the two dividers cannot
    /// both be under the pointer, so one capture overlay covers both drags.
    pub(crate) right_resize: RightResizeDrag,
    /// The right pane's last git/diff/filesystem reads, with the roots and
    /// instants they were read for. [`right::render`](crate::right::render)
    /// draws from this and never touches a subprocess or the filesystem
    /// itself.
    pub(crate) right_cache: crate::right::RightCache,
    /// A right-pane re-read is in flight on the background executor; a second
    /// one is not started on top of it.
    right_refresh_in_flight: bool,
    /// What the pane showed the last time the refresh state was reconciled:
    /// open, kind, and project root. Any change re-reads at once.
    right_last_key: Option<(bool, layout::RightKind, Option<std::path::PathBuf>)>,
    /// A session-switch restore just moved the pane: the next frame renders
    /// it at its target with no open/close animation (Z2). Set by the
    /// restore when it flips open/closed, consumed once by `render` through
    /// the shell's `resizing` bypass. User toggles never set it, so they
    /// keep animating exactly as today.
    pub(crate) right_snap: bool,
    /// The browser pane's engines (Z7a): one live `WebviewState` per session
    /// plus the no-session/home one, created lazily, hidden on switch, never
    /// destroyed. The boot flag picks WKWebView vs the scripted page.
    pub(crate) browser: crate::browser::BrowserRegistry,
    /// The person's own Browser open still owes the URL field its focus
    /// (Z7a2). Armed by `show_right`/`toggle_right` when they land open on
    /// Browser, consumed once by the next frame's webview ensure — and only
    /// there, so activation, restore and boot never steal the keyboard into
    /// the URL field. Never persists.
    pub(crate) browser_url_focus_armed: bool,
    /// A frame-paced sidebar-wheel sweep in flight (`sidebar-scroll-sweep:`
    /// step): the in-process fallback for a real
    /// `CGEvent` gesture the environment cannot deliver. `on_frame` owns it;
    /// it never persists.
    pub(crate) sidebar_scroll_sweep: Option<crate::resize::ScrollSweep>,
    /// A frame-paced transcript-wheel sweep in flight (`transcript-scroll-
    /// sweep:` step): the transcript's twin of
    /// [`Self::sidebar_scroll_sweep`].
    pub(crate) transcript_scroll_sweep: Option<crate::resize::ScrollSweep>,
    /// The sessions list's scroll state: the caller-owned `ListState` the
    /// virtualised sidebar lays out through. A cheap
    /// handle; the component itself stays stateless. Kept in sync with the
    /// flattened rows every frame by `sync_sidebar_list`: `reset` after a
    /// regroup or filter change, `splice` after a local insert or remove,
    /// `remeasure_items` after a text-only height change — `item_count`
    /// always equals the flattened length.
    pub(crate) sidebar_list: ListState,
    /// The flattened rows the list state was last synced to, and the
    /// regroup key it was last reset for. `render_sidebar` re-flattens per
    /// frame (an index walk, no summaries cloned) and splices the
    /// difference, so a regroup is the only sync that drops the offset.
    pub(crate) prev_sidebar_rows: Vec<SidebarRow>,
    /// The grouping the rows above were flattened from, behind its cached
    /// `Rc`: a new pointer with equal rows means text changed under stable
    /// rows, which is what asks for `remeasure_items`.
    pub(crate) prev_sidebar_grouping: Option<Rc<Grouping>>,
    pub(crate) prev_sidebar_regroup: Option<SidebarRegroupKey>,
    /// The sidebar wheel's input-side state: what the
    /// capture handler writes, shared behind [`RefCell`] rather than kept
    /// on the entity. A wheel event can arrive inside a `Harness` update
    /// (a scripted `sidebar-wheel:` step dispatches from one), and updating
    /// the entity re-entrantly panics — the shared cell never borrows it,
    /// so the push is synchronous in every dispatch context and N events
    /// between paints still coalesce into one drain. `render_sidebar`
    /// applies it (drains the travel, disarms the reveal, re-arms the
    /// horizon). Deliberately outside [`crate::sidebar_view::SidebarKey`]:
    /// keyed travel would re-arm the pane per event through `on_frame`.
    pub(crate) sidebar_wheel: Rc<RefCell<SidebarWheelState>>,
    /// Whether the user has scrolled the sidebar since the reveal was last
    /// armed. A reveal never moves a user-scrolled list:
    /// the capture handler records the scroll, `render_sidebar` sets this,
    /// arming clears it, and no reveal installs while it holds.
    pub(crate) sidebar_user_scrolled: bool,
    /// The two flags a `--screenshot` wait reads out of this window (see
    /// [`crate::shot::CaptureToken`]). Handed to every session view this
    /// window opens and to `capture_and_quit`, so a second window would wait
    /// on its own.
    pub(crate) capture: CaptureToken,
    /// Whether `initialize` granted `userShell`. Requested in `conn::connect`;
    /// a server that did not grant it disables the `!` path with a banner
    /// rather than letting the command fail on the wire.
    user_shell: bool,
    /// Whether `initialize` granted `sessionMcp` (muse ≥ 1.3). Requested
    /// in `conn::connect`; a server that did not grant it opens sessions
    /// with no terminal route — the bridge rides `session/start` and
    /// `session/resume` only on the grant.
    session_mcp: bool,
    focus_root: FocusHandle,
    /// Whether an overlay stood open on the previous frame. The edge from
    /// true to false is when focus has to be parked back on the root; see
    /// the comment at that check in `render`.
    overlay_was_open: bool,
    pub(crate) focus_dialog: FocusHandle,
    pub(crate) focus_palette: FocusHandle,
    /// Set when the next frame should move the keyboard to the composer.
    pub(crate) focus_composer: bool,
    /// What the billing probe said, or `None` while it has not said it yet
    /// (spec §3.2). A probe that failed is
    /// [`Tier::Unavailable`], never `None`.
    pub(crate) tier: Option<Tier>,
    /// A probe is in flight; a second one is not started on top of it.
    pub(crate) tier_probing: bool,
    /// Baaz's own facts about each session: its name, whether it is
    /// hidden, and the title derived from its first shell command (spec §3.7).
    pub(crate) overrides: sessions::Overrides,
    /// The local record of provider-lane sessions: which provider serves
    /// each, where it ran, and when it last moved. `session/list` never
    /// names these sessions, so without this the sidebar forgets them on
    /// every restart (W5).
    pub(crate) provider_sessions: crate::provider_sessions::ProviderSessionStore,
    /// Which provider each terminal bridge session answers for, by the id
    /// the bridge names (`--session`). A Codex open mints its thread id
    /// after the bridge is already registered under the request id, so
    /// the durable record above never names that id — without this map
    /// the dock cannot tell whose agent tab it draws (T3b).
    pub(crate) terminal_providers: HashMap<String, String>,
    /// The adopted workspaces (design `docs/12-projects.md` §4).
    pub(crate) projects: Projects,
    /// The current project: the open session's project, else the last used.
    pub(crate) current_project: Option<String>,
    /// The branch behind every project group row, by project id, read on
    /// each `session/list` off the UI thread.
    pub(crate) branches: HashMap<String, String>,
    /// Whether the session list has landed at least once. The sidebar draws
    /// project groups once the index has: provisional rows debut with their
    /// groups, so the collapse measures content on its first frame instead
    /// of opening from an empty box it never re-measures.
    pub(crate) sessions_loaded: bool,
    /// Whether the session index has landed at least once: row descriptions
    /// (and with them, row visibility) come from it.
    pub(crate) index_loaded: bool,
    /// A `session/list` fetch is in flight: a second [`Self::load_sessions`]
    /// meanwhile (at boot the boot session's `session/start` lands while
    /// the probe answer's fetch is still out) sets
    /// [`Self::sessions_list_stale`] instead of fetching — the reply below
    /// issues the one follow-up. Two overlapping fetches used to apply the
    /// same reply twice, rejoining 337 rows and rebuilding the grouping and
    /// the search index for nothing.
    sessions_list_in_flight: bool,
    /// A refresh was asked for while [`Self::sessions_list_in_flight`]: the
    /// landing reply reloads once rather than dropping it.
    sessions_list_stale: bool,
    /// Window preferences: the sidebar grouping, closed groups, search scope.
    pub(crate) layout: layout::Layout,
    /// The Settings Shortcuts section's live rows, re-read when the dialog
    /// opens and after every shortcut edit. The dialog builds from state
    /// every frame, and a file read per frame is not state.
    pub(crate) shortcuts_cache: Vec<crate::keymap::EffectiveBinding>,
    /// Which Shortcuts row is capturing the next keystroke, by row id. The
    /// dialog owns no state of its own; exactly one row arms at a time.
    pub(crate) recording_shortcut: Option<String>,
    /// The last refused write per Shortcuts row, by row id: shown as the
    /// row's detail until the next edit on that row.
    pub(crate) shortcut_errors: HashMap<String, String>,
    /// The terminal tabs behind the dock, keyed by project root (D43).
    pub(crate) terminal_host: Entity<TerminalHost>,
    /// The agent's seven tools over the unix socket (D46): sessions the
    /// app registers may list, open, run, read, screen, send and close
    /// these tabs. Dropped (and its socket removed) on quit.
    pub(crate) terminal_service: TerminalService,
    /// The dock's own focus: ⌃` focuses it on open, so keys reach the pty
    /// through the wrapper until a click hands the keyboard to the grid.
    pub(crate) terminal_focus: FocusHandle,
    /// A dock resize drag in flight: the grab position and the start height,
    /// both in window pixels.
    pub(crate) terminal_drag: Option<(f32, f32)>,
    /// Whether hidden sessions are listed anyway (the Sessions menu's toggle).
    pub(crate) show_hidden: bool,
    /// Real session ids with a title generation in flight: their rows read
    /// the pending placeholder until the title lands or the attempt stands
    /// down (auto-titles).
    pub(crate) titles_pending: HashSet<String>,
    /// Every throwaway title/summary side session id this run minted, plus
    /// every `side_session` override the store held at boot. The explicit
    /// record behind the hide rule: `is_side_session` reads this and the
    /// persisted flag, never the id shape (muse 1.3.0 rejects any
    /// `session/start` id that is not its own uuid shape, so side ids carry
    /// no namespace to match on).
    pub(crate) side_sessions: HashSet<String>,
    /// Side session id → the generation it serves. Harvested off
    /// `turn/completed`, reaped by the watchdog.
    pub(crate) title_jobs: HashMap<String, titles::TitleJob>,
    /// Side session id → the byline rewrite it serves.
    pub(crate) byline_jobs: HashMap<String, titles::BylineJob>,
    /// Real session ids with a rewrite in flight: late answers for a
    /// stood-down rewrite are hidden and ignored, never landed.
    pub(crate) byline_live: HashSet<String>,
    /// Last rewrite start per real session: the 30 s debounce clock.
    pub(crate) byline_last_start: HashMap<String, std::time::Instant>,
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
    /// The Skills page state: open flag, catalog, filter, search and
    /// selection (docs/15-skills.md §4).
    pub(crate) skills: crate::skills_page::SkillsPage,
    /// The Skills page search field (⌘F on the page; name and description).
    pub(crate) skills_query: Entity<TextareaState>,
    /// The New-skill dialog's name field.
    pub(crate) skills_new_name: Entity<TextareaState>,
    /// The New-skill dialog's description field.
    pub(crate) skills_new_desc: Entity<TextareaState>,
    /// "Ask Muse to write one" (A5) arms this: the next activated session
    /// opens with the `/create-skill ` draft, never sent.
    pub(crate) pending_create_skill: bool,
    /// The pulse rings' timebase: sampled phases count cycles since here, so
    /// every dot in a frame agrees and restarts never jump.
    pub(crate) pulse_epoch: std::time::Instant,
    /// The 20 Hz pulse loop while a sampled dot is on screen; self-clearing
    /// when no dot is visible and running.
    pub(crate) pulse_task: Option<Task<()>>,
    /// A `new:` step's session is still opening: its `session/start`
    /// round-trip lands after the following steps would run. Session verbs
    /// wait for the switch (bounded) instead of acting on the session that
    /// is still open; any activation clears it.
    pub(crate) session_switch_pending: bool,
    /// Which provider open the app asked for last: every `open_on_provider`
    /// / reopen / fork call bumps this and carries its value into the
    /// background work, and `finish_provider_open` lands only the current
    /// one — a late-finishing earlier open never steals focus or the send.
    /// Scripted (synchronous) opens bump it too, so the count also tells
    /// how many children one action spawned.
    /// Z4: a provider open that failed after the click already moved. The
    /// window sits on the failed session with an inline failure instead of
    /// another session's transcript: `active` is None while this stands —
    /// the previous view parks in `session_cache` like any switch away —
    /// and the next successful activation clears it. `session_id` is the
    /// clicked session for a reopen, `None` for a fresh open that never
    /// earned an id.
    pub(crate) provider_open_error: Option<ProviderOpenError>,
    pub(crate) provider_open_epoch: u64,
    /// A one-shot: the next `finish_provider_open` lands its view with the
    /// disabled-provider banner instead of a live child start having
    /// happened (set when opening on a disabled provider, which routes
    /// through the scripted lane).
    pub(crate) pending_disabled_notice: Option<ProviderId>,
    /// The one `new_session` a provider switch is allowed to start while a
    /// switch is still pending: `SwitchProvider` / `NewSessionOnProvider`
    /// close the old view synchronously but start its replacement on a
    /// task, so they claim the next start up front — any other `new` while
    /// a switch is in flight is a duplicate and starts nothing. The claim
    /// carries the switch epoch that stamped it, so a superseded switch's
    /// still-queued task cannot steal its replacement's claim.
    pub(crate) switch_claim: Option<(String, u64)>,
    /// Switch generation (Y2b2): bumped on every `SwitchProvider` start
    /// and on every cancellation. The switch's open carries the value, and
    /// only an open whose epoch is still current may `close_replaced` and
    /// activate as the switch result — anything the person does meanwhile
    /// cancels the switch (bumping this) and the stale open is discarded.
    pub(crate) switch_epoch: u64,
    /// The view a provider switch is replacing (Y2b): the old session id,
    /// kept active and drawn — composer locked, chip already on the pick —
    /// until the replacement activates in the same update. `None` outside
    /// a switch.
    pub(crate) replacing: Option<String>,
    /// The old view's provider, to restore the chip when the open fails.
    pub(crate) replacing_provider: Option<String>,
    /// The drafts-map project that named the replaced view, to restore it
    /// when the open fails.
    pub(crate) replacing_draft_project: Option<String>,
    /// The switch epoch stamped when `replacing` was set: `activate` only
    /// honours the marker while it still matches `switch_epoch`, so a view
    /// that lands after a cancel or a superseding switch never closes a
    /// view it did not replace. `None` outside a switch, like `replacing`.
    pub(crate) replacing_epoch: Option<u64>,
    /// One handoff run per source session: the machine in
    /// [`crate::handoff`]. The source view mirrors the run's card; this
    /// map is the authority the ack and cancel paths advance.
    pub(crate) handoffs: HashMap<String, crate::handoff::HandoffRun>,
    /// Owner-epoch counter for handoffs: every request bumps it and
    /// carries the value, so an event from a superseded epoch is ignored
    /// rather than applied to a newer run.
    pub(crate) handoff_epoch: u64,
    /// The destination open in flight for a handoff: consumed when the
    /// fresh session lands, then the pack submits onto it.
    pub(crate) pending_handoff: Option<crate::handoff::PendingHandoff>,
    /// The confirm dialog's frozen facts, held while it is open.
    pub(crate) handoff_confirm: Option<crate::handoff::HandoffConfirmState>,
    /// Whether the boot session has been attempted: [`Harness::ensure_boot_session`]
    /// opens `--session`/`--send`/`--steps`' first session without waiting
    /// for `session/list`, at most once — a failed attempt must not retry on
    /// every frame, and a later list reply must not open a second session
    /// beside it (see [`Harness::open_boot_session`]'s own guard).
    pub(crate) boot_session_attempted: bool,
    /// The project being renamed through the header crumb's field: the same
    /// `rename` field does it, and `ConfirmRename` commits the project when
    /// this is set rather than the session row.
    pub(crate) renaming_project: Option<String>,
    /// Whether the project menu's Colour submenu hangs open.
    pub(crate) project_colour_open: bool,
    /// Shell trigger bounds in window coordinates, recorded once per frame
    /// by `on_children_prepainted` wrappers on the triggers themselves — what
    /// every shell menu seats at through `aui::overlay::anchored_menu`
    /// (menus anchor to their triggers, never constants).
    /// Each wrapper notifies only on its first bounds (nothing → something),
    /// so a menu opened before the first prepaint appears on the next frame
    /// instead of flashing at a wrong seat, and nothing repaints afterwards.
    /// The sidebar footer row: the account menu's trigger.
    pub(crate) footer_bounds: Option<Bounds<Pixels>>,
    /// The fixed Sessions caption row: the Sessions view menu's trigger.
    pub(crate) sessions_caption: Option<Bounds<Pixels>>,
    /// The header project crumb: the header project menu's trigger.
    pub(crate) crumb_bounds: Option<Bounds<Pixels>>,
    /// The header overflow `…` button: the overflow menu's trigger.
    pub(crate) overflow_bounds: Option<Bounds<Pixels>>,
    /// The open project menu's own rect: the Colour submenu's anchor is
    /// computed off its right edge at the Colour row's height.
    pub(crate) project_menu_bounds: Option<Bounds<Pixels>>,
    /// Every rendered project group row's tray `…` button bounds, keyed by
    /// group id, from `SidebarView::on_group_menu_prepainted` — what a group
    /// row's project menu seats at. Entries are refreshed while their row is
    /// rendered and kept afterwards, so a menu opened for a rendered-then-
    /// scrolled group still seats where the row was.
    pub(crate) group_menu_bounds: HashMap<String, Bounds<Pixels>>,
    /// The Projects palette's query field: the query filters both sections
    /// by name and path, synchronously — a dozen adopted roots and a dozen
    /// recent workspaces need no background task.
    pub(crate) projects_query: Entity<TextareaState>,
    /// The Commands palette's query field: the query filters every command
    /// on its slash and its description, synchronously — twenty-three rows
    /// need no background task.
    pub(crate) commands_query: Entity<TextareaState>,
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
    /// The sidebar row under the pointer, if any: the hover detail's owner.
    /// Written only by the rows' own hover reports (see
    /// [`Harness::note_row_hover`]); pane renders never touch it.
    pub(crate) hovered_row: Option<String>,
    /// When the current hover started: the detail opens past
    /// [`aui::nav::SESSION_DETAIL_DELAY`], so a pointer travelling past
    /// rows never flashes the card.
    pub(crate) hover_since: Option<std::time::Instant>,
    /// The hover card is past its delay and showing.
    pub(crate) hover_shown: bool,
    /// Where the showing card seats: the hovered row's own window bounds,
    /// carried by the row's hover report and frozen so the card never chases
    /// the mouse. The seat itself hangs off [`Harness::sidebar_bounds`].
    pub(crate) hover_trigger: Option<Bounds<Pixels>>,
    /// The laid-out sidebar pane's window bounds, from the pane's own
    /// prepaint in [`Harness::render_sidebar`]: what the hover card's side
    /// seat hangs off (the pane's right edge plus the card gap).
    pub(crate) sidebar_bounds: Option<Bounds<Pixels>>,
    /// Capture aid (`row-detail:<id>`): pins the hover card open for one
    /// row, seated at the selected row's bounds — free, no pointer.
    pub(crate) forced_detail: Option<String>,
    /// The selected row's window bounds, from the virtual list's
    /// `on_selected_prepainted`: what the pinned card seats at.
    pub(crate) selected_row_bounds: Option<(String, Bounds<Pixels>)>,
    /// Hover-trace state (`BAAZ_HOVER_TRACE=1`, diagnosis only): the last
    /// card result `render_row_detail` returned, so the trace logs only on
    /// change.
    pub(crate) hover_trace_last_shown: Option<bool>,
    pub(crate) tasks: Vec<Task<()>>,
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
        crate::log::boot_mark("harness-new-start");
        // The rename field is one visual line: soft wrap off, so a long name
        // scrolls under the caret instead of spilling a second line.
        let rename = cx.new(|cx| {
            let mut state = composer_state_rows("Name this session", 1, 1, window, cx);
            state.set_soft_wrap(false, window, cx);
            state
        });
        let search_query = cx.new(|cx| composer_state_rows("Search sessions and created files", 1, 1, window, cx));
        let projects_query = cx.new(|cx| composer_state_rows("Add or switch project", 1, 1, window, cx));
        let commands_query = cx.new(|cx| composer_state_rows("Every command in this build", 1, 1, window, cx));
        let skills_query = cx.new(|cx| composer_state_rows("Search skills", 1, 1, window, cx));
        let skills_new_name = cx.new(|cx| composer_state_rows("Skill name", 1, 1, window, cx));
        let skills_new_desc = cx.new(|cx| composer_state_rows("What it does and when to use it", 3, 6, window, cx));
        // The sidebar column's own view: the weak handle
        // is this Baaz entity under construction, which `cx.entity()` already
        // names inside the builder.
        let baaz_weak = cx.entity().downgrade();
        let sidebar_pane = cx.new(move |_| SidebarPane::new(baaz_weak));
        // The API-key field: masked, with the capture-safe placeholder. Enter
        // inside it submits (single-line inputs always emit `PressEnter`).
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste your key"));
        // The divider's last settled x, or the default for a fresh store.
        let restored = layout::sidebar_width(&layout::read());
        // The right divider's last settled width, or the default.
        let right_restored = layout::right_width(&layout::read());
        // The backend new sessions start on: an explicit `--provider` or
        // `BAAZ_PROVIDER` names this run; otherwise the last pick the
        // person made, kept in the store across relaunches; otherwise the
        // command-line default.
        // The terminal service binds before the first frame: its socket is
        // how the agent's routes reach the tabs, and a relay started
        // beside the app must find it already listening.
        let terminal_host = cx.new(|_| TerminalHost::new());
        // Tests isolate the socket per Harness (a unique short dir plus a
        // unique fake pid): no two Harnesses in one test process share a
        // name, so none races for ownership.
        let terminal_service = match args.terminal_socket_dir.clone() {
            Some(dir) => TerminalService::start_at(
                terminal_host.clone(),
                &dir,
                crate::terminal::service::next_isolated_pid(),
            ),
            None => TerminalService::start(terminal_host.clone(), &crate::store::support_dir()),
        };
        // Where this window's agent tools listen: the relay T2 spawns is
        // pointed at exactly this path, so a second window keeping another
        // window's name shows up here rather than as a silent misroute.
        crate::baaz_log!("terminal service socket: {}", terminal_service.socket_path().display());
        let new_provider = if args.provider_explicit {
            args.provider.clone()
        } else {
            crate::providers::read_last_provider()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_else(|| args.provider.clone())
        };
        // Z7a, read once at boot: captures (`--screenshot`) and tests run the
        // scripted browser page — a native view never appears in a gpui
        // screenshot, and captures must be deterministic — while the app runs
        // WKWebView. Never sniffed in render.
        let browser_fake = args.screenshot.is_some();
        let mut this = Self {
            new_provider,
            args,
            client: None,
            provider: None,
            provider_factory: crate::providers::default_provider_factory(
                terminal_service.socket_path().to_path_buf(),
            ),
            wire: Wire::Connecting,
            auth: Auth::Probing,
            login: Login::new(api_key.clone()),
            show_connect: false,
            connect_statuses: Vec::new(),
            connect_notes: HashMap::new(),
            codex_login: crate::connect::CodexLogin::Idle,
            codex_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            muse_sheet: false,
            provider_banners: HashMap::new(),
            seen_connected: HashSet::new(),
            pending_prefill: None,
            pending_codex_start: false,
            connect_cache_mtime: None,
            sessions: Vec::new(),
            list_epoch: 0,
            list_cache: RefCell::new(ListCache::default()),
            chain_index: RefCell::new(None),
            chain_index_builds: std::cell::Cell::new(0),
            index: HashMap::new(),
            active: None,
            pending_id: None,
            reveal: None,
            reveal_unknown: None,
            sidebar_pane,
            sidebar_key: None,
            session_cache: Vec::new(),
            drafts: HashMap::new(),
            pending_draft: None,
            starting_root: None,
            overlays: cx.new(|_| Overlays::default()),
            sidebar_open: true,
            resize: ResizeDrag::restored(restored),
            right_resize: RightResizeDrag::restored(right_restored),
            right_cache: crate::right::RightCache::default(),
            browser: crate::browser::BrowserRegistry::new(browser_fake),
            browser_url_focus_armed: false,
            right_refresh_in_flight: false,
            right_last_key: None,
            right_snap: false,
            sidebar_scroll_sweep: None,
            transcript_scroll_sweep: None,
            sidebar_list: sidebar_list_state(0),
            prev_sidebar_rows: Vec::new(),
            prev_sidebar_grouping: None,
            prev_sidebar_regroup: None,
            sidebar_wheel: Rc::new(RefCell::new(SidebarWheelState::default())),
            sidebar_user_scrolled: false,
            capture,
            user_shell: true,
            session_mcp: false,
            focus_root: cx.focus_handle(),
            overlay_was_open: false,
            focus_dialog: cx.focus_handle(),
            focus_palette: cx.focus_handle(),
            focus_composer: true,
            tier: None,
            tier_probing: false,
            overrides: sessions::Overrides::new(),
            provider_sessions: crate::provider_sessions::read(),
            terminal_providers: HashMap::new(),
            projects: Projects::default(),
            current_project: None,
            branches: HashMap::new(),
            sessions_loaded: false,
            index_loaded: false,
            sessions_list_in_flight: false,
            sessions_list_stale: false,
            layout: layout::read(),
            shortcuts_cache: crate::keymap::effective_bindings(),
            recording_shortcut: None,
            shortcut_errors: HashMap::new(),
            terminal_host,
            terminal_service,
            terminal_focus: cx.focus_handle(),
            terminal_drag: None,
            show_hidden: false,
            show_empty: false,
            show_archived: false,
            titles_pending: HashSet::new(),
            side_sessions: HashSet::new(),
            title_jobs: HashMap::new(),
            byline_jobs: HashMap::new(),
            byline_live: HashSet::new(),
            byline_last_start: HashMap::new(),
            search_query: search_query.clone(),
            search_sessions: Vec::new(),
            search_files: Vec::new(),
            search_epoch: 0,
            renaming: None,
            rename: rename.clone(),
            skills: crate::skills_page::SkillsPage::default(),
            skills_query: skills_query.clone(),
            skills_new_name: skills_new_name.clone(),
            skills_new_desc: skills_new_desc.clone(),
            pending_create_skill: false,
            pulse_epoch: std::time::Instant::now(),
            pulse_task: None,
            session_switch_pending: false,
            provider_open_error: None,
            provider_open_epoch: 0,
            pending_disabled_notice: None,
            switch_claim: None,
            switch_epoch: 0,
            replacing: None,
            replacing_provider: None,
            replacing_draft_project: None,
            replacing_epoch: None,
            handoffs: HashMap::new(),
            handoff_epoch: 0,
            pending_handoff: None,
            handoff_confirm: None,
            boot_session_attempted: false,
            renaming_project: None,
            project_colour_open: false,
            footer_bounds: None,
            sessions_caption: None,
            crumb_bounds: None,
            overflow_bounds: None,
            project_menu_bounds: None,
            group_menu_bounds: HashMap::new(),
            projects_query: projects_query.clone(),
            commands_query: commands_query.clone(),
            titled: std::collections::HashSet::new(),
            undo_stack: Vec::new(),
            window_title: None,
            window_title_key: None,
            search_status: RefCell::new(None),
            send_anyway: false,
            hovered_row: None,
            hover_since: None,
            hover_shown: false,
            hover_trigger: None,
            sidebar_bounds: None,
            forced_detail: None,
            selected_row_bounds: None,
            hover_trace_last_shown: None,
            tasks: Vec::new(),
            subscriptions: Vec::new(),
        };
        // When the agent opens a URL the Browser pane opens on it — never
        // focused, so the person's keyboard stays where it was while they
        // watch. Deferred for the same re-entrancy reason as the dock hook
        // below: the hook runs inside `drain`, inside a Harness update.
        let browser_harness = cx.entity();
        this.terminal_service.set_browser_open_hook(move |cx: &mut App, session: String| {
            let harness = browser_harness.clone();
            cx.defer(move |cx| {
                harness.update(cx, |harness, cx| {
                    harness.show_browser_for_agent(&session, cx);
                });
            });
        });
        // When the agent runs something the dock opens — never focused, so
        // the person's keyboard stays where it was while they watch.
        let harness = cx.entity();
        this.terminal_service.set_activity_hook(move |cx: &mut App| {
            // Deferred: the hook runs inside `drain`, which the pump task
            // calls inside a Harness update — touching the Harness here
            // re-enters it and aborts (`cannot update Harness while it is
            // already being updated`). The defer runs at the end of the
            // effect cycle, with the Harness off the stack.
            let harness = harness.clone();
            cx.defer(move |cx| {
                harness.update(cx, |harness, cx| {
                    harness.layout.terminal_open = true;
                    layout::write(&harness.layout);
                    cx.notify();
                });
            });
        });
        // The service's requests queue on socket threads; this pump runs
        // them on the UI thread, one pass every 15 ms, until the window is
        // gone. A pass is one `drain`: queued tools answer and waiting runs
        // are polled, so a long run never stalls a frame.
        this.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(15)).await;
                let alive = this.update(cx, |this, cx| {
                    this.terminal_service.drain(cx);
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
        // Typing in the rename field redraws the row being renamed — in the
        // sidebar pane, which owns that element, as well as here for the
        // header title that shares the field.
        this.subscriptions.push(cx.subscribe(&rename, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sidebar_pane.update(cx, |_, cx| cx.notify());
                cx.notify();
            }
        }));
        // Typing in the search palette's query re-queries off the UI thread.
        this.subscriptions.push(cx.subscribe(&search_query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.refresh_search(cx);
            }
        }));
        // Typing in the Projects palette's query only redraws: both sections
        // are filtered synchronously at render, so the selection is clamped
        // to the filtered rows here.
        this.subscriptions.push(cx.subscribe(&projects_query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.clamp_projects_selection(cx);
                cx.notify();
            }
        }));
        // Typing in the Commands palette's query only redraws: the rows are
        // filtered synchronously at render, so the selection is clamped to
        // the filtered rows here.
        this.subscriptions.push(cx.subscribe(&commands_query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.clamp_commands_selection(cx);
                cx.notify();
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
        // A restart mid-flight still hides every side session: the persisted
        // `side_session` flags rejoin the in-memory record at boot.
        this.side_sessions = this
            .overrides
            .iter()
            .filter(|(_, meta)| meta.side_session)
            .map(|(id, _)| id.clone())
            .collect();
        // Before anything reads it: "New session" with no project starts
        // here, and on a first launch that click comes before the person has
        // adopted anything at all.
        projects::ensure_default_workspace();
        this.boot_projects();
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
            // A scripted sidebar draws without a list reply — but only when
            // one was asked for, so the login captures draw as they did.
            if this.args.sidebar_fixture.is_some() {
                this.apply_sidebar_fixture();
                this.sessions_loaded = true;
                this.index_loaded = true;
                this.invalidate_list();
            }
            return this;
        }
        // The launch decision from stored facts only: a first run shows the
        // Connect your providers screen; every other launch renders the
        // shell at once from the cached statuses. Muse's `account/read`
        // still reports (and refreshes Muse's row), but never gates the
        // window.
        this.show_connect = crate::connect::decide_launch(crate::provider_status::is_first_run())
            == crate::connect::LaunchDecision::Connect;
        this.connect_statuses = crate::connect::initial_connect_statuses();
        this.connect_cache_mtime = crate::connect::cache_mtime();
        this.connect(cx);
        this.load_index(cx);
        // Only when a project is current. With none — a first launch, or a
        // bundle opened from Finder, where `boot_projects` declined to adopt
        // `/` — `workspace()` falls back to the launch directory, which is not
        // a workspace and holds nothing worth priming a picker with. Adopting
        // a project, or starting a session in a folder, loads them then.
        if this.current_project.is_some() {
            this.load_menu_sources(std::path::PathBuf::from(this.workspace()), cx);
        }
        crate::log::boot_mark("harness-new-done");
        this
    }

    /// The workspace path as the wire and the header want it: the current
    /// project's root, else the launch workspace.
    pub(crate) fn workspace(&self) -> String {
        self.current_project()
            .map(|p| p.root.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.args.workspace.to_string_lossy().into_owned())
    }

    /// The current project's id, when one is current.
    pub(crate) fn current_project_id(&self) -> Option<String> {
        self.current_project.clone()
    }

    /// Record one shell trigger's window bounds, notifying only when they
    /// arrive for the first time: a menu opened before the first prepaint
    /// appears on the next frame instead of flashing at a wrong seat, and
    /// steady bounds never schedule work of their own.
    pub(crate) fn note_trigger_bounds(
        slot: &mut Option<Bounds<Pixels>>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Harness>,
    ) {
        let had = slot.is_some();
        *slot = Some(bounds);
        if !had {
            cx.notify();
        }
    }

    /// The current project: the open session's project, else the last used —
    /// else the most recently opened adoption whose root is on disk. A
    /// missing root is never current, but stays adopted.
    pub(crate) fn current_project(&self) -> Option<&Project> {
        self.current_project
            .as_deref()
            .and_then(|id| self.projects.find_available(id))
            .or_else(|| self.projects.most_recent_available())
    }

    /// The header and window title for a chain head: the collapsed view
    /// row's label — the same one the sidebar's one row wears — so header
    /// and row can never disagree (Y2a3). Falls back to the stored row's
    /// title ladder when the head is filtered out of the view.
    pub(crate) fn collapsed_head_label(&self, head: &str, cx: &gpui::App) -> Option<String> {
        if let Some(entry) = self.visible_sessions(cx).iter().find(|e| e.id == head) {
            let pending = entry.title_pending || self.titles_pending.contains(&entry.id);
            return Some(crate::sidebar::display_label(&entry.label, pending).to_owned());
        }
        self.sessions.iter().find(|e| e.id == head).map(|e| {
            let pending = e.title_pending || self.titles_pending.contains(&e.id);
            let text = if !e.named {
                sidebar::handoff_title_of(head, &self.provider_sessions, &self.overrides)
                    .map(|t| sidebar::one_line(&t))
                    .unwrap_or_else(|| e.label.clone())
            } else {
                e.label.clone()
            };
            crate::sidebar::display_label(&text, pending).to_owned()
        })
    }

    /// What the window's own title bar says: the open session against its
    /// project, the project alone, or the app name with no project at all.
    fn window_title(&self, cx: &gpui::App) -> String {
        let session = self.active.as_ref().map(|a| a.read(cx).session_id.clone());
        // One identity per chain: the raw view id resolves to its head,
        // and the collapsed view row — the same label the sidebar's one
        // row wears — names the window (Y2a, Y2a3).
        let label = session.and_then(|id| self.collapsed_head_label(&self.chain_head(&id), cx));
        match self.current_project() {
            Some(project) => match label {
                Some(label) => format!("{label} \u{2014} {}", project.name),
                None => project.name.clone(),
            },
            None => "Baaz".to_owned(),
        }
    }

    /// The current project's name, which is what the header and the empty
    /// state show — or the app name when no project is current.
    fn workspace_name(&self) -> String {
        self.current_project().map(|p| p.name.clone()).unwrap_or_else(|| "Baaz".to_owned())
    }

    /// Read the projects store and settle the current project (decision D39):
    /// an explicit `--workspace` wins and is adopted if new; a scripted run's
    /// launch directory keeps today's semantics by the same rule; else the
    /// stored current project if it still exists; else the most recently
    /// opened adoption; else the launch directory, unless it is `/` or
    /// `$HOME`, where the window opens with no project and the hero owns the
    /// empty state. The file is written when anything above changed it.
    pub(super) fn boot_projects(&mut self) {
        // `--no-project` boots the hero: nothing adopted, nothing current.
        if self.args.no_project {
            self.projects = Projects::default();
            self.current_project = None;
            return;
        }
        self.projects = projects::read();
        let scripted = self.args.replay.is_some()
            || self.args.offline
            || !self.args.steps.is_empty()
            || self.args.screenshot.is_some();
        let mut dirty = false;
        // A scripted run adopts where it was launched, so a capture works in
        // the checkout it was started from. It is still subject to D39: a
        // bundle opened from Finder starts at `/`, and adopting that would
        // both contradict the rule below and write a junk project. An
        // explicit `--workspace` is explicit intent and wins either way.
        let launch_adoptable = projects::is_workspace_root(&self.args.workspace);
        if self.args.workspace_explicit || (scripted && launch_adoptable) {
            let workspace = self.args.workspace.clone();
            let id = self.projects.add(&workspace).id.clone();
            self.projects.touch(&id);
            if self.projects.current.as_deref() != Some(id.as_str()) {
                self.projects.current = Some(id);
            }
            dirty = true;
        } else if self.projects.current.as_deref().is_some_and(|id| self.projects.find_available(id).is_some()) {
            // The stored current project still exists on disk: keep it, untouched.
            // A missing root is not current (it stays adopted, and comes back
            // when the path does).
        } else if let Some(recent) = self.projects.most_recent_available().map(|p| p.id.clone()) {
            self.projects.current = Some(recent);
            dirty = true;
        } else {
            let launch = self.args.workspace.clone();
            if projects::is_workspace_root(&launch) {
                let id = self.projects.add(&launch).id.clone();
                self.projects.touch(&id);
                self.projects.current = Some(id);
                dirty = true;
            } else if self.projects.current.is_some() {
                self.projects.current = None;
                dirty = true;
            }
        }
        self.current_project = self.projects.current.clone();
        if dirty {
            projects::write(&self.projects);
        }
    }

    // ------------------------------------------------------------ connection

    /// Spawn `muse serve`, initialize, and start draining its events.
    fn connect(&mut self, cx: &mut Context<Self>) {
        crate::log::boot_mark("connect-sent");
        let program = self.args.program.clone();
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                let out = conn::connect(&program);
                crate::log::boot_mark(&format!("connect-work-done ok={} in={}ms", out.is_ok(), at.elapsed().as_millis()));
                out
            },
            |this, result, cx| match result {
            Ok(connected) => {
                crate::log::boot_mark("connect-reply");
                    crate::baaz_log!("connected to {} {}", connected.legacy.agent_name, connected.legacy.agent_version);
                if let Some(warning) = &connected.legacy.warning {
                    // A fingerprint mismatch is additive evolution, never a
                    // failure: say so on stderr and carry on.
                    crate::baaz_log!("{warning}");
                }
                this.user_shell = connected.legacy.user_shell;
                this.session_mcp = connected.legacy.session_mcp;
                this.client = Some(connected.legacy.transport);
                this.provider = Some(Arc::new(Mutex::new(connected.provider)));
                this.wire = Wire::Ready;
                this.pump(connected.legacy.events, cx);
                this.observe_provider(connected.events, cx);
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

    /// Observe the provider stream alongside the legacy pump: the legacy
    /// path still owns reconnect (on the transport's close) and rendering
    /// (through the session views), so this task only notices a lost
    /// connection for the log. It ends when the provider is dropped.
    fn observe_provider(&mut self, mut events: UnboundedReceiver<ProviderEvent>, cx: &mut Context<Self>) {
        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if matches!(event, ProviderEvent::ConnectionLost { .. }) {
                    crate::baaz_log!("provider reported the connection lost");
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            }
        }));
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
            // The exit reason, while the old client still owns it: its
            // status and whatever it said on stderr last. Read before
            // `reconnect` drops the client.
            let tail = self.client.as_ref().map(|client| client.stderr_tail()).unwrap_or_default();
            if tail.is_empty() {
                crate::baaz_log!("muse serve exited ({code:?}); reconnecting");
            } else {
                crate::baaz_log!("muse serve exited ({code:?}); reconnecting; stderr tail: {tail}");
            }
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
            // `usage/changed` keeps the tier live past its boot-time reading:
            // the footer meter and the banner follow through `push_tier`. A
            // frame that does not decode keeps the known tier.
            if method == "usage/changed" {
                self.apply_usage_changed(params, cx);
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
        // A loaded session's projected `(status, attention)` flipped: the
        // command-plane broadcast (muse 1.3.0) that keeps rows other than
        // the open session fresh without a `session/list` round trip.
        // Extracted before the view takes the event, applied after.
        let status_changed: Option<(String, bool, Option<Vec<muse_client::schema::AttentionFlag>>)> = match &event {
            MuseEvent::Notification { method, params, session_id, .. } if method == "session/statusChanged" => {
                serde_json::from_value::<muse_client::schema::SessionStatusChangedParams>(params.clone())
                    .ok()
                    .map(|change| {
                        let id = session_id.clone().unwrap_or(change.session_id.clone());
                        let running = matches!(change.status, muse_client::schema::SessionStatus::Running);
                        (id, running, change.attention)
                    })
            }
            _ => None,
        };
        // A turn's terminal: which session, whether it failed, and the
        // failure's message when it did. Recorded into the row (and the
        // store) after `apply`, so `Failed` survives the session closing.
        let turn_outcome: Option<(String, bool, Option<String>)> = match &event {
            MuseEvent::Notification { method, params, session_id, .. } if method == "turn/completed" => {
                let id = session_id
                    .clone()
                    .or_else(|| params.get("sessionId").and_then(|v| v.as_str()).map(str::to_owned));
                let failed = params.get("terminal").and_then(|v| v.as_str()).is_some_and(|t| t == "failed");
                let error = params
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                id.map(|id| (id, failed, error))
            }
            _ => None,
        };
        // The row's live facts, straight from the event — extracted before
        // the view takes the event below, applied after, so the fold
        // already holds the event's world (see the call past `apply`).
        let live: Option<(bool, String)> = match &event {
            MuseEvent::Notification { method, params, session_id, .. } => {
                let id = session_id
                    .clone()
                    .or_else(|| params.get("sessionId").and_then(|v| v.as_str()).map(str::to_owned));
                match (method.as_str(), id) {
                    ("turn/started", Some(id)) => Some((true, id)),
                    ("turn/completed", Some(id))
                    | ("approval/requested", Some(id))
                    | ("approval/updated", Some(id))
                    | ("approval/resolved", Some(id))
                    | ("userInput/requested", Some(id))
                    | ("userInput/settled", Some(id)) => Some((false, id)),
                    _ => None,
                }
            }
            _ => None,
        };
        // A side session's turn completed: harvest its answer for the title
        // it serves. The open view ignores foreign ids on apply, so this is
        // the only place a side session's events land.
        if let MuseEvent::Notification { method, session_id, .. } = &event {
            if method == "turn/completed" {
                if let Some(side_id) = session_id.clone() {
                    if self.title_jobs.contains_key(&side_id) {
                        self.harvest_title(&side_id, cx);
                    } else if self.byline_jobs.contains_key(&side_id) {
                        self.harvest_byline(&side_id, cx);
                    }
                }
            }
        }
        if let Some(active) = &self.active {
            active.update(cx, |view, cx| view.apply(event, cx));
        }
        // The row's live facts, straight from the event — no `session/list`
        // round trip: a started turn reads running now, a completed one (or
        // an approval/question event) re-reads the open view's pending
        // words. Runs after `apply`, so the fold already holds the event's
        // world.
        if let Some((is_start, id)) = live {
            self.sync_row_live(&id, is_start, cx);
        }
        // The broadcast's `(status, attention)`, folded into the row for
        // sessions whose view is not open; the open session then overlays
        // its own truth, which is fresher than any listing.
        if let Some((id, running, attention)) = status_changed {
            if !self.is_side_session(&id) {
                if let Some(entry) = self.sessions.iter_mut().find(|entry| entry.id == id) {
                    entry.apply_status_changed(running, attention);
                    self.invalidate_list();
                }
            }
            self.sync_row_live(&id, false, cx);
        }
        // The turn's terminal, recorded into the row and the store: a
        // failed turn's message is what `Failed` stands on, and a later
        // success (or a retry's start) stands it down.
        if let Some((id, failed, error)) = turn_outcome {
            self.record_turn_outcome(&id, failed, error.as_deref(), cx);
        }
        if let Some(session_id) = started {
            let prompt = self
                .active
                .as_ref()
                .filter(|view| view.read(cx).session_id == session_id)
                .and_then(|view| view.read(cx).first_prompt_text());
            // A turn that started is a session made real: it is no draft
            // any more, whether it already had a row or not.
            self.drafts.retain(|_, named| named != &session_id);
            if let Some(entry) = self.sessions.iter_mut().find(|entry| entry.id == session_id) {
                // muse 1.3.0 lists a zero-turn session in `session/list`
                // like any other row, so the draft this turn is making real
                // may already be a wire entry here rather than the `local`
                // placeholder the `else` arm below still covers — either
                // shape is invisible (`SessionEntry::is_empty`) until
                // `first_send_update` runs (see its doc).
                let handoff_dest =
                    sidebar::is_handoff_dest(&session_id, &self.provider_sessions, &self.overrides);
                if sidebar::first_send_update(entry, prompt.as_deref(), crate::clock::now_local(), handoff_dest) {
                    self.invalidate_list();
                    // The row just went from invisible to visible. A reveal
                    // armed when this session was opened may already have
                    // given up on it: `reveal_sidebar_row` disarms on the
                    // first miss for a *known* session with nowhere to go,
                    // which this row was — present in `self.sessions`, but
                    // filtered as empty — until the line above. Re-arm it
                    // exactly as an outside activation does, so the sidebar
                    // still steers to the row a first send just created; but
                    // only for the session this window has open, so a turn
                    // on some other session never steals its reveal.
                    if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == session_id) {
                        self.reveal = Some(session_id.clone());
                        self.reveal_unknown = None;
                    }
                }
            } else if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == session_id) {
                // The first send of this window's rowless draft: insert its
                // local row now, titled from the prompt, newest-dated for
                // the top of its project group. A turn no open view sent
                // names nothing this window can title, so it inserts no row.
                let project = self.overrides.get(&session_id).and_then(|m| m.project.clone());
                let workspace = self.session_workspace(&session_id);
                // A handoff destination's rowless first turn is the pack, not
                // the person's words: the row carries the chain title (Y2a).
                let label = if sidebar::is_handoff_dest(&session_id, &self.provider_sessions, &self.overrides) {
                    sidebar::handoff_title_of(&session_id, &self.provider_sessions, &self.overrides)
                        .map(|t| sidebar::one_line(&t))
                        .unwrap_or_else(|| sidebar::UNNAMED.to_owned())
                } else {
                    prompt.filter(|prompt| !prompt.is_empty()).unwrap_or_else(|| sidebar::UNNAMED.to_owned())
                };
                let row = sidebar::local_started_row(
                    &session_id,
                    label,
                    project,
                    Some(workspace),
                    crate::clock::now_local(),
                );
                self.sessions.retain(|entry| entry.id != session_id);
                self.sessions.push(row);
                self.invalidate_list();
            }
            // A first send may earn a generated title: one cheap model call
            // in a throwaway side session, never blocking this turn. Only
            // the open view's own turn qualifies — its prompt is the title's
            // source, and a turn no open view sent names nothing this window
            // can title.
            if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == session_id) {
                let prompt = self.active.as_ref().and_then(|view| view.read(cx).first_prompt_text());
                self.maybe_start_title(&session_id, prompt, cx);
            }
        }
        if completed {
            self.load_index(cx);
            self.load_sessions(cx);
            self.record_last_summary(cx);
            // The free excerpt just landed; a poor one may earn one
            // debounced rewrite — idle sessions only, never a running turn.
            self.maybe_rewrite_byline(cx);
        }
        self.title_from_transcript(cx);
        cx.notify();
    }

    /// The reconnect procedure: respawn, `initialize`, then `session/resume`
    /// from the last observed cursor, which serves `history.mode: "none"` and
    /// streams only the suffix.
    ///
    /// Two phases, split at the handshake. A successful
    /// `initialize` is `Wire::Ready` no matter what the resume then says: a
    /// resume rejection about the session (another window holds the lease,
    /// the session is gone) is not a transport failure, so it becomes a
    /// banner on that session's view while the sidebar, the palette and ⌘N
    /// keep working. Only a failed respawn takes the wire down.
    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        self.provider = None;
        let program = self.args.program.clone();
        let resume = self
            .active
            .as_ref()
            .map(|a| (a.read(cx).session_id.clone(), a.read(cx).last_cursor()));
        let work = move || conn::connect(&program);
        self.wire_call(cx, work, move |this, result, cx| match result {
            Ok(connected) => {
                if let Some(warning) = &connected.legacy.warning {
                    // A fingerprint mismatch is additive evolution, never a
                    // failure: say so on stderr and carry on.
                    crate::baaz_log!("{warning}");
                }
                // Re-seat the views before the provider moves: dropping the
                // last legacy transport ends the old pump, so the old
                // adapter detaches cleanly when it follows.
                let transport = connected.legacy.transport;
                this.session_mcp = connected.legacy.session_mcp;
                if let Some(active) = &this.active {
                    active.update(cx, |view, cx| view.reconnected(transport.clone(), cx));
                }
                this.client = Some(transport);
                this.provider = Some(Arc::new(Mutex::new(connected.provider)));
                this.wire = Wire::Ready;
                this.pump(connected.legacy.events, cx);
                this.observe_provider(connected.events, cx);
                this.resume_after_reconnect(resume, cx);
                cx.notify();
            }
            Err(error) => {
                this.wire = Wire::Down(error.to_string());
                this.set_dialog(cx, Dialog {
                    title: conn::provider_title(&error),
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

    /// The reconnect's second phase: re-attach the session that was open.
    ///
    /// Runs after the handshake settled, on the live child. Success clears
    /// the lease notice a previous rejection left; a session-scoped rejection
    /// banners that session's view and marks it read-only until a later
    /// resume succeeds; a stale sidecar logs one line (the history is intact
    /// and the next open retries the lease); anything else drops the wire as
    /// before.
    fn resume_after_reconnect(
        &mut self,
        resume: Option<(String, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, cursor)) = resume else { return };
        let Some(client) = self.client.clone() else { return };
        let resumed_id = session_id.clone();
        let mut resume_params = SessionResumeParams {
            command_id: new_command_id(),
            session_id: session_id.clone(),
            cursor: cursor.clone(),
            exclude_items: Some(true),
            history: None,
            config: None,
        };
        // The terminal relay's route, as on every resume: re-registered
        // and carried, grant-gated.
        self.muse_terminal_resume(&mut resume_params);
        let work = move || client.session_resume(&resume_params);
        self.wire_call(cx, work, move |this, result, cx| {
            // The notice belongs to the resumed session: a switch since owns
            // its own lease, so anything but the still-open resumed view is
            // left alone.
            let open = this.active.clone().filter(|view| view.read(cx).session_id == resumed_id);
            match result {
                Ok(_) => {
                    if let Some(view) = open {
                        view.update(cx, |view, cx| view.note_resumed(cx));
                    }
                    cx.notify();
                }
                Err(error) if error.is_stale_sidecar() => {
                    crate::baaz_log!("reconnect resume hit a stale sidecar ({error}); history stays, lease retries on next open");
                    cx.notify();
                }
                Err(error) if conn::is_session_scoped(&error) => {
                    let banner = conn::lease_banner(&error);
                    crate::baaz_log!("reconnect resume rejected ({error}); banner on the view, wire stays up");
                    if let Some(view) = open {
                        view.update(cx, |view, cx| view.set_lease_lost(&banner, cx));
                    }
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
            }
        });
    }

    // ---------------------------------------------------------- billing tier

    // ---------------------------------------------------------------- render

    /// Whether this window draws settled rather than entering: a
    /// `--screenshot` run, or a deterministic capture
    /// (`BAAZ_DETERMINISTIC=1`), which is always a static composition even
    /// without a screenshot on the end.
    pub(crate) fn still(&self) -> bool {
        self.args.screenshot.is_some() || crate::clock::deterministic()
    }

    /// The sidebar's collapse toggle: the rail is the column at zero width.
    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// The dock toggle (⌃`): flips the open state, persists it, and focuses
    /// the dock on open so keys reach the pty.
    pub(crate) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout.terminal_open = !self.layout.terminal_open;
        if self.layout.terminal_open {
            self.ensure_terminal_tab(cx);
            window.focus(&self.terminal_focus, cx);
        }
        layout::write(&self.layout);
        cx.notify();
    }

    /// Trust `session_id` for the terminal service (D53): the app opened
    /// this session, so its agent routes may drive the project's tabs.
    /// Called wherever a session becomes current; the provider lanes join
    /// in T2, which owns their open paths — and their closes, which is
    /// where the matching unregister will live. No session-close path runs
    /// in T1b's scope (hide/archive/undo all close views outside it), so an
    /// unregister here would be dead code; until T2 the registry lives as
    /// long as the window, and the socket's removal on quit ends it.
    pub(crate) fn register_terminal_session(
        &mut self,
        session_id: &str,
        project_root: std::path::PathBuf,
        provider: &str,
    ) {
        self.terminal_service.register_session(session_id, project_root);
        self.terminal_providers.insert(session_id.to_owned(), provider.to_owned());
    }

    /// The right-pane toggle (⌘⌥B, and the header's PanelRight button):
    /// flips the open state, persists it, notifies, and re-reads the pane's
    /// data off the render path when it ends up open.
    pub(crate) fn toggle_right(&mut self, cx: &mut Context<Self>) {
        // A user toggle always animates, even when a restore armed the
        // snap and no frame has consumed it yet.
        self.right_snap = false;
        self.layout.right_open = !self.layout.right_open;
        crate::baaz_log!("toggle right pane: open={}", self.layout.right_open);
        self.arm_browser_url_focus();
        layout::write(&self.layout);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
        cx.notify();
    }

    /// Open the right pane on `kind`: records the kind, forces the pane
    /// open, persists, notifies. Task T3 calls this from the ⌘K palette, so
    /// it works with no session open and never touches [`Self::active`].
    /// Calling it with the kind already showing while the pane is open
    /// closes the pane again, the way pressing a menu's own button closes it.
    /// Either way the pane's data is re-read off the render path when it ends
    /// up open.
    pub(crate) fn show_right(&mut self, kind: layout::RightKind, cx: &mut Context<Self>) {
        // A user pick always animates (see `toggle_right`).
        self.right_snap = false;
        if self.layout.right_open && self.layout.right_kind == Some(kind) {
            self.layout.right_open = false;
        } else {
            self.layout.right_kind = Some(kind);
            self.layout.right_open = true;
        }
        crate::baaz_log!("show right pane: {kind:?} open={}", self.layout.right_open);
        self.arm_browser_url_focus();
        layout::write(&self.layout);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
        cx.notify();
    }

    /// Arm the URL-field focus the next Browser frame owes (Z7a2): only a
    /// person's own open — a click, the palette, a shortcut — earns it, and
    /// only landing open on Browser. Restores never arm it, so a restored
    /// Browser pane leaves the keyboard where activation put it.
    fn arm_browser_url_focus(&mut self) {
        if self.layout.right_open && layout::right_kind(&self.layout) == layout::RightKind::Browser {
            self.browser_url_focus_armed = true;
        }
    }

    /// Save the live pane state onto the active session (Z2): open, kind,
    /// and the Files preview/selection/expansion for the current project.
    /// Called on every user change while a session is active — toggle,
    /// show, close, preview, select, expand/collapse, the matching `--steps`
    /// verbs. With no session active (home/empty state) this is a no-op and
    /// today's global `layout.json` behaviour stands. `right_width` stays
    /// global and is never saved here.
    pub(crate) fn save_right_for_active(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let session_id = view.read(cx).session_id.clone();
        let (files_preview, files_selected, files_expanded) = match self.right_project() {
            Some((root, _)) => {
                let preview = self.right_cache.preview_for(&root).map(|preview| preview.path.clone());
                let selected = self.right_cache.selected_for(&root);
                let mut expanded: Vec<String> =
                    self.right_cache.expanded_for(&root).into_iter().collect();
                expanded.sort();
                (preview, selected, expanded)
            }
            None => (None, None, Vec::new()),
        };
        // Z7a: the session's last browser URL rides along untouched — the
        // navigation sync owns it, and a pane toggle must never clear it.
        let browser_url = self
            .overrides
            .get(&session_id)
            .and_then(|meta| meta.right.clone())
            .and_then(|right| right.browser_url);
        let state = crate::sessions::RightState {
            open: self.layout.right_open,
            kind: layout::right_kind(&self.layout),
            files_preview,
            files_selected,
            files_expanded,
            browser_url,
        };
        self.set_override(&session_id, |meta| meta.right = Some(state), cx);
    }

    /// Restore the live pane state from `session_id`'s stored [`RightState`]
    /// (Z2): what a session view's activation calls, so every switch — click,
    /// resume, reopen, provider reopen, handoff landing, launch restore —
    /// shows the pane exactly as that session left it. A session with no
    /// stored state shows the pane closed, as does a brand-new session. The
    /// Files preview/selection/expansion restore onto the current project
    /// root (the tree listing/diff/git caches stay per root), and the pane's
    /// data refreshes. Flipping open/closed arms [`Self::right_snap`] so the
    /// next frame lands with no animation; user toggles never arm it.
    pub(crate) fn restore_right_for_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let stored = self.overrides.get(session_id).and_then(|meta| meta.right.clone());
        let open = stored.as_ref().is_some_and(|state| state.open);
        if self.layout.right_open != open {
            self.layout.right_open = open;
            self.right_snap = true;
        }
        if let Some(state) = &stored {
            self.layout.right_kind = Some(state.kind);
        }
        if let Some((root, _)) = self.right_project() {
            match &stored {
                Some(state) => {
                    let expanded: std::collections::HashSet<String> =
                        state.files_expanded.iter().cloned().collect();
                    if expanded.is_empty() {
                        self.right_cache.expanded.remove(&root);
                    } else {
                        self.right_cache.expanded.insert(root.clone(), expanded);
                    }
                    match &state.files_selected {
                        Some(selected) => {
                            self.right_cache.selected.insert(root.clone(), selected.clone());
                        }
                        None => {
                            self.right_cache.selected.remove(&root);
                        }
                    }
                    match &state.files_preview {
                        Some(preview) => {
                            let preview = preview.clone();
                            self.begin_file_preview_for(&root, &preview, cx);
                        }
                        None => {
                            crate::right::close_file_preview(&mut self.right_cache, &root);
                        }
                    }
                }
                None => {
                    self.right_cache.expanded.remove(&root);
                    self.right_cache.selected.remove(&root);
                    crate::right::close_file_preview(&mut self.right_cache, &root);
                }
            }
        }
        self.refresh_right_now(cx);
    }

    /// How often the open pane re-reads git and the filesystem while it sits
    /// open on a kind that reads them: at most once per interval.
    const RIGHT_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

    /// Whether `kind` shows data read from git or the filesystem: everything
    /// but the Browser.
    fn right_needs_data(kind: layout::RightKind) -> bool {
        !matches!(kind, layout::RightKind::Browser)
    }

    /// The project the right pane reads for: root plus display name, from the
    /// in-memory current project — never the projects store on disk, which the
    /// old render path re-read on every frame.
    pub(crate) fn right_project(&self) -> Option<(std::path::PathBuf, String)> {
        self.current_project().map(|project| (project.root.clone(), project.name.clone()))
    }

    /// Re-read git and the filesystem for the open pane, off the render path.
    /// No-op while the pane is closed, on Browser, or with no project. At most
    /// one re-read is ever in flight: an explicit request (opening, a kind
    /// switch, the file tree's Refresh) while one runs is coalesced, and the
    /// interval in [`Self::sync_right_cache`] paces the steady state. Under
    /// `BAAZ_DETERMINISTIC` the read is fixtures plus a capped walk and runs
    /// inline, so captures settle on their first frame; otherwise it runs on
    /// the background executor and lands with an update + notify. Nothing here
    /// blocks the UI thread.
    pub(crate) fn refresh_right_now(&mut self, cx: &mut Context<Self>) {
        let kind = layout::right_kind(&self.layout);
        if !self.layout.right_open || !Self::right_needs_data(kind) {
            return;
        }
        let Some((root, _)) = self.right_project() else { return };
        // The walk draws around this: a re-read never collapses a toggled
        // directory or drops the preview's tree.
        let expanded = self.right_cache.expanded_for(&root);
        if crate::clock::deterministic() {
            let at = std::time::Instant::now();
            let snapshot = {
                let dir_cache = self.right_cache.dir_cache_for_root(&root);
                crate::right::read_snapshot_for_cached(&root, &expanded, dir_cache)
            };
            let reload = self
                .right_cache
                .preview_for(&root)
                .and_then(|preview| crate::right::preview_reload_for(&root, &preview));
            self.right_cache.apply_snapshot(snapshot, at);
            if let Some(reload) = reload {
                crate::right::apply_preview_reload(&mut self.right_cache, &root, reload);
            }
            self.right_last_key = Some((true, kind, Some(root)));
            cx.notify();
            return;
        }
        if self.right_refresh_in_flight {
            return;
        }
        self.right_refresh_in_flight = true;
        // The listing cache and the open preview travel into the background
        // task: unchanged directories are replayed there, and a preview whose
        // file changed on disk is re-read there — never on the render path.
        let dir_cache = self.right_cache.take_dir_cache();
        let preview = self.right_cache.preview_for(&root);
        cx.spawn(async move |this, cx| {
            let (snapshot, dir_cache, reload) = cx
                .background_executor()
                .spawn(async move {
                    let mut dir_cache = dir_cache;
                    let snapshot =
                        crate::right::read_snapshot_for_cached(&root, &expanded, &mut dir_cache);
                    let reload = preview
                        .as_ref()
                        .and_then(|preview| crate::right::preview_reload_for(&root, preview));
                    (snapshot, dir_cache, reload)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.right_refresh_in_flight = false;
                this.right_cache.restore_dir_cache(dir_cache);
                let root = snapshot.root.clone();
                this.right_cache.apply_snapshot(snapshot, std::time::Instant::now());
                if let Some(reload) = reload {
                    crate::right::apply_preview_reload(&mut this.right_cache, &root, reload);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reconcile the refresh state every frame: re-read at once when the pane
    /// opened, its kind changed, or its project changed; re-read on the
    /// interval while it sits open on a data kind; never while closed or on
    /// Browser. The `right:` step verb writes the layout directly, so it is
    /// covered here rather than at a call site.
    fn sync_right_cache(&mut self, cx: &mut Context<Self>) {
        let kind = layout::right_kind(&self.layout);
        let open = self.layout.right_open;
        let root = self.right_project().map(|(root, _)| root);
        if !open || !Self::right_needs_data(kind) {
            self.right_last_key = Some((open, kind, root));
            return;
        }
        if self.right_last_key != Some((open, kind, root.clone())) {
            self.right_last_key = Some((open, kind, root));
            self.refresh_right_now(cx);
            return;
        }
        // The steady state, paced off when the slot for this kind last
        // landed: never fetched re-reads at once, a fresh landing waits out
        // the interval, and a hung in-flight read is never doubled.
        let Some(root) = root else { return };
        let due = self
            .right_cache
            .fetched_at(kind, &root)
            .is_none_or(|at| at.elapsed() >= Self::RIGHT_REFRESH_INTERVAL);
        if due && !self.right_refresh_in_flight {
            self.refresh_right_now(cx);
        }
    }

    /// A new terminal tab on the current project, opening the dock for it.
    ///
    /// Goes through D43's [`pick`](terminal::TerminalHost::pick) with a
    /// `"new"` route, so the rule the unit tests pin is the rule this runs.
    pub(crate) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout.terminal_open = true;
        if let Some(root) = self.current_project().map(|project| project.root.clone()) {
            let decision = self.terminal_host.read(cx).pick(cx, &root, false);
            match decision {
                terminal::Pick::New => {
                    let origin = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
                    let title = terminal::title_from_command("shell");
                    self.terminal_host.update(cx, |host, cx| {
                        host.open(&root, title, TabOwner::User, origin, cx);
                    });
                }
                terminal::Pick::Existing(id) => {
                    self.terminal_host.update(cx, |host, _| host.activate(&id));
                }
            }
        }
        window.focus(&self.terminal_focus, cx);
        layout::write(&self.layout);
        cx.notify();
    }

    /// D51 "Open terminal" on a terminal tool card: open the dock and
    /// focus the card's tab. A tab the host no longer holds (or none
    /// named) opens the dock on its active tab instead of failing.
    pub(crate) fn open_terminal_tab(
        &mut self,
        tab: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.terminal_open = true;
        if let Some(tab) = tab {
            if self.terminal_host.read(cx).get(&tab).is_some() {
                self.terminal_host.update(cx, |host, _| host.activate(&tab));
            }
        }
        window.focus(&self.terminal_focus, cx);
        layout::write(&self.layout);
        cx.notify();
    }

    /// The first open creates the project's tab; later opens keep it. Tabs
    /// belong to the project, not the session, so this never runs twice for
    /// one project in a window's life (D43).
    fn ensure_terminal_tab(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.current_project().map(|project| project.root.clone()) else { return };
        if !self.terminal_host.read(cx).tabs_for(&root).is_empty() {
            return;
        }
        let origin = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
        self.terminal_host.update(cx, |host, cx| {
            host.open(&root, "shell".to_owned(), TabOwner::User, origin, cx);
        });
    }

    /// ⌃C while the dock holds the keyboard: SIGINT to the project's active
    /// tab, where ⌃C while the composer holds it stops the running turn.
    fn terminal_sigint(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.current_project().map(|project| project.root.clone()) else { return };
        let session = self.terminal_host.read(cx).active_for(&root).map(|tab| tab.session.clone());
        if let Some(session) = session {
            session.update(cx, |session, _| session.write(b"\x03"));
        }
    }

    /// Point the project's active tab at its n-th tab, in open order.
    fn activate_terminal_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(root) = self.current_project().map(|project| project.root.clone()) else { return };
        let id = self.terminal_host.read(cx).tabs_for(&root).get(index).map(|tab| tab.id.clone());
        if let Some(id) = id {
            self.terminal_host.update(cx, |host, _| host.activate(&id));
            cx.notify();
        }
    }

    /// Close the project's n-th tab, in open order — asking first when it
    /// is busy (D52), naming the running command. An idle tab closes
    /// outright.
    fn close_terminal_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(root) = self.current_project().map(|project| project.root.clone()) else { return };
        let id = self.terminal_host.read(cx).tabs_for(&root).get(index).map(|tab| tab.id.clone());
        if let Some(id) = id {
            let running = self.terminal_host.read(cx).running_command(cx, &id);
            if let Some(dialog) = Self::close_tab_dialog(&id, running.as_deref()) {
                self.set_dialog(cx, dialog);
            } else {
                self.terminal_host.update(cx, |host, _| host.close(&id));
            }
            cx.notify();
        }
    }

    /// One block hover action from the active tab's grid (D44). The library
    /// performs none of these — it only reports the intent with a block
    /// index, and the host reads the block back and decides. Nothing here
    /// touches the wire: no turn, no send.
    fn handle_terminal_intent(
        &mut self,
        tab_id: &str,
        intent: TerminalGridIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            TerminalGridIntent::OpenUrl(url) => {
                // A program can print any OSC 8 link it likes, so only
                // `http`/`https` ever reach the browser — the rest are
                // ignored (D53).
                if terminal::intents::openable_url(&url) {
                    cx.open_url(&url);
                }
            }
            TerminalGridIntent::Copy(block) => {
                let text = self
                    .terminal_host
                    .read(cx)
                    .get(tab_id)
                    .and_then(|tab| tab.session.read(cx).block_text(block));
                if let Some(text) = text {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }
            TerminalGridIntent::Stop(block) => {
                // A finished (or missing) block is a no-op; a running one
                // takes `⌃C`.
                let running = self.terminal_host.read(cx).get(tab_id).is_some_and(|tab| {
                    tab.session.read(cx).blocks().get(block).is_some_and(|block| block.running())
                });
                if running {
                    let session =
                        self.terminal_host.read(cx).get(tab_id).map(|tab| tab.session.clone());
                    if let Some(session) = session {
                        session.update(cx, |session, _| session.write(b"\x03"));
                    }
                }
            }
            TerminalGridIntent::Rerun(block) => self.rerun_block(tab_id, block, window, cx),
            TerminalGridIntent::Ask(block) => self.ask_about_block(tab_id, block, window, cx),
        }
    }

    /// Rerun a block's command: the same tab when it is idle, otherwise a
    /// new one — [`terminal::host::pick_tab`]'s rule (D43) with the
    /// originating tab as the candidate. The dock open, paste, Enter and
    /// focus below are the play buttons' own entry point
    /// ([`Harness::run_in_terminal`]).
    fn rerun_block(&mut self, tab_id: &str, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.current_project().map(|project| project.root.clone()) else { return };
        let command = self
            .terminal_host
            .read(cx)
            .get(tab_id)
            .and_then(|tab| tab.session.read(cx).blocks().get(block).map(|block| block.command.clone()))
            .filter(|command| !command.trim().is_empty());
        let Some(command) = command else { return };
        let pick = self.terminal_host.read(cx).pick_rerun(cx, &root, tab_id);
        self.run_in_picked(&root, pick, &command, true, window, cx);
    }

    /// Ask about a block: its command and capped ANSI-free output land in
    /// the current session's composer draft as a quoted block — and stay
    /// there unsent. An occupied composer is appended to, never clobbered.
    fn ask_about_block(&mut self, tab_id: &str, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else {
            self.overlays.update(cx, |overlays, _| {
                overlays.toast("No open session", "Open a session to ask about this terminal output.");
            });
            cx.notify();
            return;
        };
        let payload = self.terminal_host.read(cx).get(tab_id).and_then(|tab| {
            let session = tab.session.read(cx);
            let command = session.blocks().get(block).map(|block| block.command.clone())?;
            let output = session.block_text(block)?;
            Some((command, output))
        });
        let Some((command, output)) = payload else { return };
        // `set_draft` only: the draft is never sent from here.
        view.update(cx, |view, cx| {
            let existing = view.draft_text(cx);
            view.set_draft(terminal::intents::build_ask_draft(&command, &output, &existing), window, cx);
        });
    }

    /// The `--steps` verb's route: open the dock over a FakePty-backed tab
    /// and drain its script at once, so the capture replays the same bytes
    /// on every run. The payload names the tab; empty is "terminal".
    pub(crate) fn step_terminal_dock(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let title = rest.trim();
        let title = if title.is_empty() { "terminal".to_owned() } else { title.to_owned() };
        let root = self
            .current_project()
            .map(|project| project.root.clone())
            .unwrap_or_else(|| PathBuf::from(self.workspace()));
        self.layout.terminal_open = true;
        let origin = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
        let nonce = aui_terminal::generate_nonce();
        let script = terminal::deterministic_script(&nonce);
        self.terminal_host.update(cx, |host, cx| {
            let id = host.open_fake(&root, title, TabOwner::User, origin, script, &nonce, cx);
            host.drain(&id, cx);
        });
        layout::write(&self.layout);
        window.focus(&self.terminal_focus, cx);
        cx.notify();
    }

    /// The centre column's height: the window minus the 44 px header cell.
    fn centre_height(&self, window: &Window) -> f32 {
        f32::from(window.bounds().size.height).max(0.0) - 44.0
    }

    /// The dock's height now: the stored one, clamped into `[120px, 70% of
    /// the centre column]`.
    fn dock_height(&self, window: &Window) -> f32 {
        let want = self.layout.terminal_height.unwrap_or(terminal::DOCK_DEFAULT_HEIGHT);
        terminal::clamp_dock_height(want, self.centre_height(window))
    }

    /// The terminal dock under the composer (D42): the library's
    /// `terminal_dock` frame over `terminal_tabs` and the active tab's
    /// `terminal_grid`, or the empty state when the project has no tab.
    ///
    /// Opens and closes on the layout spring like the right pane: the outer
    /// height springs between the resting height and zero while the body
    /// inside keeps its resting height, so the grid is clipped — never
    /// re-laid-out — and the pty sees no resize storm mid-motion. The dock
    /// stays mounted while it collapses and leaves the tree only once the
    /// spring has settled shut.
    /// The agent mark for a terminal tab: the provider of the session that
    /// opened it — Claude Code's C, Codex's O — and `None` when the
    /// person opened it or no record names the lane (D43). The strip
    /// still wears Muse's M for an unknown tab, which is what the
    /// deterministic capture's baseline holds; only the dock hint goes
    /// neutral there.
    fn terminal_tab_provider(&self, origin: Option<&str>) -> Option<Provider> {
        origin
            .and_then(|session| self.provider_sessions.get(session))
            .and_then(|record| crate::sidebar::provider_mark(&record.provider))
            .or_else(|| {
                origin
                    .and_then(|session| self.terminal_providers.get(session))
                    .and_then(|provider| crate::sidebar::provider_mark(provider))
            })
    }

    /// The dock header's hint for the active tab's agent: whose session the
    /// person shares the terminal with (D43). Muse keeps the exact string
    /// the baselines hold; other lanes name their own agent; a tab no
    /// session owns names none, rather than wearing Muse's name.
    fn terminal_hint_name(&self, provider: Option<Provider>) -> String {
        match provider {
            Some(Provider::Muse) => "Muse can type here".to_owned(),
            Some(Provider::Claude) => "Claude Code can type here".to_owned(),
            Some(Provider::Codex) => "Codex can type here".to_owned(),
            _ => "The agent can type here".to_owned(),
        }
    }

    fn render_terminal_dock(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.layout.terminal_open;
        let resting = self.dock_height(window);
        let target = if open { px(resting) } else { px(0.0) };
        // Mid-drag the height feeds straight through: `spring_px` would chase
        // a moving target and the divider would lag the pointer. On release
        // the spring re-arms from the current height, so there is no jump.
        // `terminal_drag` is the dock's equivalent of the columns' `resizing`.
        let shown = if self.terminal_drag.is_some() {
            target
        } else {
            spring_px(("terminal-dock", "dock-height"), target, SpringKind::Layout, window, cx).max(px(0.0))
        };
        if !open && shown <= px(1.0) {
            return None;
        }
        let root = self.current_project().map(|project| project.root.clone())?;
        let height = resting;
        let host = self.terminal_host.read(cx);
        let mut tabs = Vec::new();
        let mut active_ix = 0;
        let mut active_tab: Option<(String, Entity<aui_terminal::TerminalSession>)> = None;
        let mut hint_provider = None;
        let active_id = host.active_for(&root).map(|tab| tab.id.clone());
        for tab in host.tabs_for(&root) {
            let provider = self.terminal_tab_provider(tab.origin_session.as_deref());
            if Some(tab.id.as_str()) == active_id.as_deref() {
                active_ix = tabs.len();
                active_tab = Some((tab.id.clone(), tab.session.clone()));
                hint_provider = provider;
            }
            let mut view = TermTab::new(tab.id.clone(), tab.title.clone());
            if tab.owner == TabOwner::Agent {
                view = view.agent(provider.unwrap_or(Provider::Muse));
            }
            if host.busy(cx, &tab.id) {
                view = view.busy(true);
            }
            tabs.push(view);
        }
        let select = cx.processor(|this: &mut Self, action: TerminalTabsAction, window, cx| match action {
            TerminalTabsAction::Select(index) => this.activate_terminal_tab(index, cx),
            TerminalTabsAction::Close(index) => this.close_terminal_tab(index, cx),
            TerminalTabsAction::New => this.new_terminal(window, cx),
        });
        let header = terminal_tabs("terminal-tabs", tabs, active_ix).on_action(select);
        let body = active_tab.map(|(tab_id, session)| {
            let intent = cx.processor(move |this: &mut Self, intent: TerminalGridIntent, window, cx| {
                this.handle_terminal_intent(&tab_id, intent, window, cx);
            });
            // The grid draws its cursor focused — and blinks it — only while
            // the handle it was GIVEN is focused. Hand it the dock's own
            // handle, the one `track_focus` binds below and ⌃` focuses, or
            // the grid falls back to a handle nothing in this app ever
            // focuses and the cursor stays hollow and still forever.
            terminal_grid(&session)
                .focus_handle(self.terminal_focus.clone())
                .on_intent(intent)
                .into_any_element()
        });
        let maximised = height >= terminal::clamp_dock_height(f32::MAX, self.centre_height(window)) - 0.5;
        let dock_action =
            cx.processor(move |this: &mut Self, action: TerminalDockAction, window, cx| match action {
                TerminalDockAction::Maximize => {
                    let max = terminal::clamp_dock_height(f32::MAX, this.centre_height(window));
                    this.layout.terminal_height =
                        if maximised { Some(terminal::DOCK_DEFAULT_HEIGHT) } else { Some(max) };
                    layout::write(&this.layout);
                    cx.notify();
                }
                TerminalDockAction::Close => {
                    this.layout.terminal_open = false;
                    layout::write(&this.layout);
                    cx.notify();
                }
                TerminalDockAction::NewTerminal => this.new_terminal(window, cx),
            });
        let dock = terminal_dock("terminal-dock", header, body)
            .hint(self.terminal_hint_name(hint_provider))
            .maximized(maximised)
            .on_action(dock_action);
        let resize_start = cx.processor(|this: &mut Self, grab: f32, _, _| {
            this.terminal_drag = Some((grab, this.layout.terminal_height.unwrap_or(terminal::DOCK_DEFAULT_HEIGHT)));
        });
        let resize_move = cx.processor(|this: &mut Self, at: f32, window, cx| {
            if let Some((grab, start)) = this.terminal_drag {
                this.layout.terminal_height =
                    Some(terminal::clamp_dock_height(start + (grab - at), this.centre_height(window)));
                cx.notify();
            }
        });
        let end_weak = cx.entity().downgrade();
        let resize_end = move |_: &mut Window, cx: &mut App| {
            end_weak
                .update(cx, |this: &mut Harness, cx| {
                    this.terminal_drag = None;
                    layout::write(&this.layout);
                    cx.notify();
                })
                .ok();
        };
        let dock = dock.on_resize_start(resize_start).on_resize(resize_move).on_resize_end(resize_end);
        Some(
            div()
                .h(shown)
                .w_full()
                .flex_none()
                .overflow_hidden()
                .key_context(gpui::KeyContext::parse(TERMINAL_CONTEXT).unwrap_or_default())
                // No key handler here: the grid below is given this very
                // handle, so it IS the focus node and routes keys itself.
                // A handler here would see every keystroke a second time
                // as it bubbles, and the pty would receive `aa` for `a`.
                .track_focus(&self.terminal_focus)
                // The body keeps its resting height while the outer springs,
                // the right pane's `right_inner`: the grid measures this box,
                // so its bounds — and the pty size it reports — never move
                // mid-motion. The shrinking outer clips it instead.
                .child(div().h(px(height)).w_full().flex_none().child(dock))
                .into_any_element(),
        )
    }


    /// The centre header: the current project's crumb (`mark project ▾`),
    /// a `·` separator, the active session's label with the provider mark,
    /// and the overflow menu — and nothing else.
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
        // One identity per chain: header, window and selection all resolve
        // the raw id to its head, and the collapsed view row names it —
        // the same label the sidebar's one row wears (Y2a, Y2a3).
        let target = target.map(|id| self.chain_head(&id));
        // The Skills page names itself after the crumb: "Skills · [project]".
        let label = if self.skills.open {
            Some("Skills".to_owned())
        } else {
            target.as_deref().and_then(|id| self.collapsed_head_label(id, cx))
        };
        let overflow =
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.open_menu(MenuKind::Overflow, cx));
        // The project crumb: name and chevron as one click target that opens
        // the project menu under it — `project › session`, no marks (owner
        // round 4, O4). With no current project it names the unfiled lane —
        // where a session started right now would go — and opens the
        // Projects palette, which is how one gets filed.
        let renaming_project_here =
            self.current_project.as_deref().is_some_and(|id| self.renaming_project.as_deref() == Some(id));
        let crumb: AnyElement = if renaming_project_here {
            div().flex_none().child(self.rename_field(window, cx)).into_any_element()
        } else if let Some(project) = self.current_project() {
            let id = project.id.clone();
            let name = project.name.clone();
            let open = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                if this.renaming_project.is_some() {
                    return;
                }
                this.open_project_menu(Some(id.clone()), true, cx);
            });
            let state = interaction("hd-project", window, cx);
            // The wrapper reports the crumb's own rect: `on_children_prepainted`
            // fires with the children's bounds, and the crumb is this wrapper's
            // only child, so its first bounds are the trigger the header
            // project menu seats at.
            let crumb_report = cx.entity().downgrade();
            div()
                .flex_none()
                .on_children_prepainted(move |bounds, _, cx| {
                    if let Some(first) = bounds.first() {
                        let bounds = *first;
                        let _ = crumb_report.update(cx, |this, cx| {
                            Harness::note_trigger_bounds(&mut this.crumb_bounds, bounds, cx);
                        });
                    }
                })
                .child(
                    h_flex()
                        .id("hd-project")
                        .flex_none()
                        .items_center()
                        .gap(px(5.0))
                        .role(gpui::Role::Button)
                        .aria_label(format!("Project {name}. Open project menu"))
                        .track_interaction(&state)
                        .on_click(open)
                        .child(div().text_color(p.ink).ui(scale::FS_13).semibold().child(name))
                        .child(icon(IconName::ChevronDown).size(px(11.0)).color(p.ink_3)),
                )
                .into_any_element()
        } else {
            let open = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.open_projects(false, window, cx);
            });
            let state = interaction("hd-project", window, cx);
            h_flex()
                .id("hd-project")
                .flex_none()
                .items_center()
                .role(gpui::Role::Button)
                .aria_label("Open projects")
                .track_interaction(&state)
                .on_click(open)
                .child(div().text_color(p.ink_3).ui(scale::FS_13).semibold().child(crate::sidebar::UNFILED_LABEL))
                .into_any_element()
        };
        // The session half is the label after the `·` separator, with no
        // provider mark before it. The title flexes
        // inside the header cell and clips to one line, so a whole first
        // prompt as the derived title can never push the overflow button
        // out. There is no width token in aui-tokens, so the flex leftover
        // — not a fixed max — is the constraint, which also holds on narrow
        // windows.
        let mut title = h_flex()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .items_center()
            .gap(px(7.0))
            .text_color(p.ink)
            .ui(scale::FS_13)
            .semibold()
            .child(crumb);
        if let Some(label) = label {
            // Renaming the open session swaps the session label for the same
            // dense field the sidebar row uses (Task A); the commit path is
            // the same `ConfirmRename`, Escape the same `cancel`.
            let renaming_here = self
                .active
                .as_ref()
                .map(|view| view.read(cx).session_id.clone())
                .is_some_and(|id| self.renaming.as_deref() == Some(id.as_str()));
            title = title.child(div().flex_none().text_color(p.ink_4).child("·"));
            if renaming_here {
                // The field's own root is `w_full`, so the flex item clips:
                // the overflow button keeps its slot instead of being pushed
                // out.
                title = title.child(
                    div().flex_1().min_w(px(0.0)).overflow_hidden().child(self.rename_field(window, cx)),
                );
            } else {
                title = title.child(div().flex_1().min_w(px(0.0)).truncate().child(label));
            }
        }
        let title: AnyElement = title.into_any_element();
        // No expand button: the header row stands still while the sidebar
        // collapses, so the toggle in the sidebar header stays put and the
        // centre cell never slides under the native lights.
        let cell = header_cell("hd-centre");
        // The terminal toggle: ⌃`'s button, lit while the dock stands open.
        let term_toggle = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.toggle_terminal(window, cx);
        });
        let mut term_button = icon_button("hd-centre-terminal", IconName::Terminal)
            .ghost()
            .size(ButtonSize::Sm)
            .accessibility_label("Toggle terminal");
        if self.layout.terminal_open {
            term_button = term_button.on_click(term_toggle);
        } else {
            term_button = term_button.muted().on_click(term_toggle);
        }
        // The right-pane toggle: ⌘⌥B's button, lit while the pane stands
        // open. The centre cell is a custom `header_cell` (project crumb,
        // session rename, overflow seating), not the library's
        // `centre_header`, so the library's `on_toggle_right` has no
        // compatible builder here — and the right cell collapses to width
        // zero while closed, so a toggle placed there could close but never
        // reopen. This is the same ghost `PanelRight` the library paints,
        // wired straight to `toggle_right`, with a hover label naming what
        // it does.
        let right_toggle = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.toggle_right(cx);
        });
        let mut right_button =
            icon_button("hd-centre-toggle-right", IconName::PanelRight)
                .ghost()
                .size(ButtonSize::Sm)
                .accessibility_label("Toggle right pane");
        if self.layout.right_open {
            right_button = right_button.on_click(right_toggle);
        } else {
            right_button = right_button.muted().on_click(right_toggle);
        }
        // The tooltip names the button for a pointer; the role and label name
        // it for everything else. `icon_button` yields a `Button`, which has
        // no aria builder of its own, so the wrapper carries both.
        let right_tip = div()
            .id("hd-centre-toggle-right-tip")
            .role(gpui::Role::Button)
            .aria_label("Toggle right pane")
            .tooltip(|_, cx| cx.new(|_| RightToggleTip).into())
            .child(right_button);
        // The wrapper reports the overflow button's own rect (its only
        // child), which is what the overflow menu seats at.
        let overflow_report = cx.entity().downgrade();
        cell
            .child(title)
            .child(term_button)
            .child(
                div()
                    .flex_none()
                    .on_children_prepainted(move |bounds, _, cx| {
                        if let Some(first) = bounds.first() {
                            let bounds = *first;
                            let _ = overflow_report.update(cx, |this, cx| {
                                Harness::note_trigger_bounds(&mut this.overflow_bounds, bounds, cx);
                            });
                        }
                    })
                    .child(
                        icon_button("hd-centre-overflow", IconName::Dots)
                            .ghost()
                            .muted()
                            .size(ButtonSize::Sm)
                            .accessibility_label("Open session menu")
                            .on_click(overflow),
                    ),
            )
            .child(right_tip)
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
        // The capture's own root when it names one: the replayed row groups
        // where a live session of that root would.
        let workspace = sidebar::replay_workspace(&path)
            .map(|root| projects::canonical_str(&root))
            .unwrap_or_else(|| self.workspace());
        let (provider, workspace) = (self.args.provider.clone(), workspace);
        let overlays = self.overlays.clone();
        let capture = self.capture.clone();
        // `--bench` through the shell drives the stream itself, event by
        // event, so the view opens in bench-replay state instead of folding
        // the capture at once.
        let bench = self.args.bench_shell;
        // The capture names its own session; this id is a placeholder the view
        // replaces the moment the first line is folded.
        let view = cx.new(|cx| {
            let host = SessionHost { provider_id: provider, workspace, overlays, capture, terminal_host: None };
            let mut view = SessionView::new("replay".to_owned(), None, host, window, cx);
            view.set_at_rest(at_rest);
            if bench {
                if let Ok((events, sent)) = session::parse_replay_file(&path) {
                    let id = session::capture_session_id(&events).unwrap_or_else(|| "bench".to_owned());
                    view.begin_bench_replay(id, sent, cx);
                }
            } else {
                view.load_replay(&path, cx);
            }
            view.load_history(cx);
            view
        });
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        // The replayed row stands for the capture's own turns: without them
        // the empty filter reads it as a turn-less session and drops it the
        // moment it is not the open one — so switching sessions in a replay
        // window would shrink the list under the scroll (the click moved
        // `-1262 → -1198` on the clamp, not on the reveal).
        let mut replayed = SessionEntry::replayed(&view.read(cx).session_id, &path, &self.projects);
        replayed.turns = view.read(cx).session().map(|s| s.turns.len() as u64).unwrap_or(0);
        // The replayed row stands for the folded capture, and no wire event
        // will ever sync it the way `session/list` does live: read the open
        // session's own words off the view here — the same overlay
        // `render_row_detail` applies every frame.
        {
            let view = view.read(cx);
            let (approval, question) = view.row_pending();
            if approval.is_some() {
                replayed.approval_command = approval;
            }
            if question.is_some() {
                replayed.pending_question = question;
            }
            if replayed.last_ask.as_deref().map(str::trim).is_none_or(|s| s.is_empty()) {
                replayed.last_ask = view.last_user_text();
            }
            if replayed.description.trim().is_empty() {
                if let Some(summary) = view.last_summary_text() {
                    replayed.description = summary;
                }
            }
        }
        self.sessions = vec![replayed];
        // A scripted sidebar joins the replayed row, as if the wire had
        // listed it beside the capture.
        self.apply_sidebar_fixture();
        // The replay's one row is the whole list, and there is no index
        // to wait for: both have landed.
        self.sessions_loaded = true;
        self.index_loaded = true;
        self.invalidate_list();
        // `--session <id>`: the live boot opens it once the list arrives
        // (`load_sessions`), and a replay window has no list reply — so the
        // landed fixture is the arrival it waits for. A run
        // with no session named keeps the replayed view.
        self.open_boot_session(window, cx);
        // A replayed window has no wire, but the search palette still needs
        // the host's session index: read it (read-only) and rebuild `search.db`
        // so `--steps search:<query>` screenshots show session hits.
        self.load_index(cx);
        let tier_banner = self.tier_banner();
        view.update(cx, |view, cx| view.set_tier_banner(tier_banner, cx));
        self.active = Some(view);
        // The replayed session is current, so its agent routes may drive
        // the terminal (D53): register it for this window's project root.
        // Live lanes register on their own open paths (T2 owns those).
        let replayed = self
            .active
            .as_ref()
            .map(|view| view.read(cx))
            .map(|view| (view.session_id.clone(), view.provider_kind()));
        if let Some((session_id, provider)) = replayed {
            let root = self
                .current_project()
                .map(|project| project.root.clone())
                .unwrap_or_else(|| self.args.workspace.clone());
            self.register_terminal_session(&session_id, root, provider.as_str());
        }
        // The capture is already folded, and no wire event will ever run
        // `title_from_transcript` for it: without this the replayed row
        // keeps the file's name even when the transcript knows better.
        self.title_from_transcript(cx);
        self.maybe_run_steps(window, cx);
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
        let provider_banner = self.render_provider_banner(cx);
        // The transcript column has its own cached entity boundary (owner
        // round 6, part 4): a sidebar-only frame reuses the retained
        // transcript instead of rebuilding it. The composer band and the
        // drop overlay compose live beside it — the band's textarea would
        // pin any cached ancestor dirty on every paint, and a hidden
        // overlay would keep asking for the next frame while its exit runs.
        // `render_no_session` stays inline — there is no view to cache on,
        // and the screen is static.
        // Route::Skills: the page replaces the transcript area while open.
        let body = if self.skills.open {
            self.render_skills_page(window, cx)
        } else {
            match self.active.clone() {
            Some(view) => {
                // No `.cached(...)` while assistive tech is on: gpui-pre's reuse replays
                // hitboxes and mouse listeners but not a11y node bounds, and an AXPress is
                // a synthetic click at those bounds — so every press in a clean cached pane
                // did nothing. gpui refreshes the window when a screen reader (de)activates.
                let transcript = if window.is_a11y_active() {
                    view.clone().into_any_element()
                } else {
                    view.clone().cached(StyleRefinement::default().flex_grow(1.)).into_any_element()
                };
                let band = view.update(cx, |view, cx| view.render_composer_band(window, cx));
                let overlay = view.update(cx, |view, cx| view.render_drop_overlay(window, cx));
                v_flex()
                    .size_full()
                    .relative()
                    .child(transcript)
                    .child(band)
                    .children(overlay)
                    // gpui reports an external drag only while it moves, so
                    // that is what raises the overlay; the drop takes it
                    // down again.
                    .on_drag_move(cx.listener(
                        |this: &mut Self, _: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                            // Z7a: an external drag covers the browser pane —
                            // the native view hides while it lasts.
                            this.browser.drop_cover = true;
                            this.with_session(cx, |view, cx| view.note_drag_over(cx));
                        },
                    ))
                    .on_drop(cx.listener(|this: &mut Self, paths: &ExternalPaths, _, cx| {
                        this.browser.drop_cover = false;
                        this.with_session(cx, |view, cx| view.drop_external(paths, cx));
                    }))
                    .into_any_element()
            }
            None => match self.provider_open_error.clone() {
                // Z4: the failed session's own place — never another
                // session's transcript behind a dialog.
                Some(failure) => self.render_provider_open_error(window, cx, &failure),
                None => self.render_no_session(window, cx),
            },
            }
        };
        let dock = self.render_terminal_dock(window, cx);
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
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                this.toggle_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPane, _, cx| {
                this.toggle_right(cx);
            }))
            .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
                this.new_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &TerminalSigint, _, cx| {
                this.terminal_sigint(cx);
            }))
            // 1–9 on a pending approval: the n-th server-minted choice, in the
            // order the server sent them.
            .on_action(cx.listener(|this, nth: &aui::keys::ChooseNth, window, cx| {
                let index = nth.index;
                this.with_session(cx, |view, cx| view.choose_nth(index, window, cx));
            }))
            .children(banner)
            .children(provider_banner)
            .child(body)
            .children(dock)
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

    /// No session open: with no project at all the hero owns the state —
    /// Muse works inside a folder, and the two ways in both open the
    /// Projects palette. Otherwise the one thing to do is start one.
    /// The hero column also takes a drop: every dropped directory is
    /// adopted, the first becoming current.
    /// The pick for new sessions, changed only by the composer's provider
    /// chip — never by a live session, which keeps the lane it was created
    /// on. Remembered in the store, so the next launch starts where the
    /// person last chose.
    pub(crate) fn select_new_provider(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        // A disabled provider is never the default for new sessions: fall
        // back to the first enabled one (Muse when none is).
        let id = if crate::settings_providers::live_visible_provider_ids().contains(&id) {
            id
        } else {
            crate::settings_providers::live_first_visible().unwrap_or(ProviderId::Muse)
        };
        self.new_provider = id.as_str().to_owned();
        crate::providers::write_last_provider(id);
        cx.notify();
    }

    /// Hang up every provider lane's child this window holds — the open view
    /// and every parked one. What app quit calls so no `claude` or `codex`
    /// child outlives the app; dropping the views would do the same through
    /// [`Drop`](crate::session::SessionView), but quit should not rely on
    /// teardown order. Quitting with a switch pending cancels it too, so an
    /// open that lands after teardown is discarded instead of activating.
    pub(crate) fn shutdown_provider_views(&mut self, cx: &mut Context<Self>) {
        self.cancel_pending_switch(cx);
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, _| view.shutdown_lane());
        }
        for (_, view) in &self.session_cache {
            view.update(cx, |view, _| view.shutdown_lane());
        }
    }

    /// The inline state for a provider open that failed after the click
    /// moved (Z4): the failed title, the error, Retry + Providers…, and a
    /// disabled composer naming the provider. This is deliberately not a
    /// session view, so no transcript — and no tier banner — from anywhere
    /// else can render here.
    fn render_provider_open_error(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
        failure: &ProviderOpenError,
    ) -> AnyElement {
        let p = cx.aui().colors;
        let label = failure.provider.label();
        // The click handlers re-run the failed open, or open Settings on
        // the Providers section. Each wrapper carries the role and the
        // human label: `button` has no aria builder of its own, so the
        // header toggle wraps it the same way.
        let retry = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.retry_provider_open(window, cx);
        });
        let providers = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_providers(cx);
        });
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(scale::SP_3))
                    .pb(px(HERO_LIFT))
                    .px(px(scale::SP_7))
                    .child(
                        div()
                            .text_role(aui_tokens::TextRole::Title)
                            .text_color(p.ink)
                            .child(if failure.session_id.is_some() {
                                format!("Couldn't reopen {label}")
                            } else {
                                format!("Couldn't start {label}")
                            }),
                    )
                    .child(
                        div()
                            .ui(scale::FS_12)
                            .text_color(p.ink_3)
                            .child(failure.error.clone()),
                    )
                    .child(
                        h_flex()
                            .gap(px(scale::SP_2))
                            .mt(px(scale::SP_2))
                            .child(
                                div()
                                    .id("reopen-failed-retry")
                                    .role(gpui::Role::Button)
                                    .aria_label(format!("Retry reopening {label}"))
                                    .child(button("reopen-retry", "Retry").primary().on_click(retry)),
                            )
                            .child(
                                div()
                                    .id("reopen-failed-providers")
                                    .role(gpui::Role::Button)
                                    .aria_label("Open Providers settings")
                                    .child(
                                        button("reopen-providers", "Providers…").on_click(providers),
                                    ),
                            ),
                    ),
            )
            .child(
                div().w_full().bg(p.surface_1).border_t_1().border_color(p.line).child(
                    div()
                        .w_full()
                        .px(px(scale::SP_7))
                        .py(px(scale::SP_4))
                        .ui(scale::FS_12)
                        .text_color(p.ink_4)
                        .child(format!("{label} is not available")),
                ),
            )
            .into_any_element()
    }

    fn render_no_session(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        if self.current_project().is_none() {
            let start = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.new_session(window, cx)
            });
            let choose = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.choose_project_folder(cx));
            let drop = cx.listener(|this: &mut Self, paths: &ExternalPaths, _, cx| {
                this.adopt_dropped(paths.paths().to_vec(), cx);
            });
            // Nothing adopted is not a dead end any more: the default
            // workspace means a first launch can ask something straight
            // away, and adopting a folder is the other thing it can do
            // rather than the only one.
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(scale::SP_3))
                .pb(px(HERO_LIFT))
                .on_drop(drop)
                .child(crate::mascot::boot_mascot(window, cx))
                .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink_2).child("Ready when you are"))
                .child(
                    div()
                        .ui(scale::FS_12)
                        .text_color(p.ink_3)
                        .child("Start a session now, or add a project folder to work in."),
                )
                .child(
                    h_flex()
                        .gap(px(scale::SP_2))
                        .mt(px(scale::SP_2))
                        .child(button("hero-new", "New session").primary().icon(IconName::Plus).on_click(start))
                        .child(button("hero-choose", "Add project").icon(IconName::Folder).on_click(choose)),
                )
                .into_any_element();
        }
        let new = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.new_session(window, cx));
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(scale::SP_4))
            .pb(px(HERO_LIFT))
            .child(crate::mascot::boot_mascot(window, cx))
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
    /// `rest` is `<session_id>:<workspace-relative path>`. The join is
    /// against the hit's own workspace — the session's, not this window's —
    /// so a file created in project A reveals under A while B is current. A
    /// path that no longer exists is a toast, not a reveal of whatever
    /// happens to sit at the workspace root.
    pub(crate) fn reveal_created(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Some((session_id, path)) = rest.split_once(':') else { return };
        let workspace =
            self.sessions.iter().find(|e| e.id == session_id).and_then(|e| e.workspace.clone());
        let full = match workspace {
            Some(root) => std::path::PathBuf::from(root).join(path),
            None => self.args.workspace.join(path),
        };
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
        let scoped = !self.layout.search_all_projects;
        let project = self.current_project().map(|p| p.name.clone()).unwrap_or_default();
        let key = (blank, self.search_sessions.len(), self.search_files.len(), scoped, project.clone());
        let mut cache = self.search_status.borrow_mut();
        if let Some((cached, status)) = cache.as_ref() {
            if *cached == key {
                return status.clone();
            }
        }
        let (_, sessions, files, _, _) = key;
        let status: SharedString = if blank {
            // A palette narrowed to the current project says whose.
            if scoped && !project.is_empty() {
                format!("Search {project}…").into()
            } else {
                "Search sessions and created files".into()
            }
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
/// The hover label on the header's right-pane toggle: what it does, not
/// what it is.
struct RightToggleTip;

impl gpui::Render for RightToggleTip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let p = cx.aui().colors;
        div()
            .px(px(scale::SP_3))
            .py(px(scale::SP_2))
            .rounded(px(scale::R_SM))
            .border_1()
            .border_color(p.line_strong)
            .bg(p.overlay)
            .text_color(p.ink)
            .ui(scale::FS_12)
            .whitespace_nowrap()
            .child("Toggle right pane")
    }
}

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
        // A running dot advances on the pulse loop's ticks, not on animation
        // frames: make sure the loop is up exactly while a sampled dot is on
        // screen. Cheap when it is already running (one `is_some`) and when
        // nobody runs (one scan).
        self.ensure_pulse_task(cx);
        // A regained window focus re-probes the provider statuses (Y4):
        // edge-triggered inside, so steady active frames do no work of
        // their own and probes run at most every 15s per provider.
        crate::provider_status::note_window_active(window.is_window_active());
        // The window's title is the session's, so a person with three baaz
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
        // A scripted drag has no pointer to release, so only its own
        // `resize-end` settles it — otherwise a headless (never active)
        // window could never hold one across frames.
        if self.resize.active && !self.resize.scripted && !window.is_window_active() {
            self.resize.active = false;
            self.resize.persist();
        }
        // The right drag's twin: a release outside the window never reaches
        // the overlay either, so an armed right drag settles where it stands
        // when the window goes inactive.
        if self.right_resize.active && !self.right_resize.scripted && !window.is_window_active() {
            self.right_resize.active = false;
            self.right_resize.persist();
        }
        // A frame-paced width sweep marches the divider one step per
        // rendered frame (scripting only): each tick logs
        // its width plus the renders and re-hints it cost, so per-tick
        // resize cost is readable off a scripted run. Like a real drag it
        // owns the list (no reveal installs while active); unlike one it
        // never persists — a measurement, not a choice.
        if self.resize.sweep.is_some() {
            let from = self.resize.width;
            let next = self.resize.sweep.as_mut().map(|sweep| sweep.advance(from)).unwrap_or(from);
            self.resize.width = next;
            let ticks = self.resize.sweep.as_ref().map(|sweep| sweep.ticks).unwrap_or(0);
            let pane = crate::sidebar_view::take_sidebar_pane_renders();
            let root = crate::sidebar_view::take_baaz_root_renders();
            let rehint = self
                .active
                .clone()
                .map(|view| view.update(cx, |view, _| view.take_trace_rehint()))
                .unwrap_or(false);
            crate::baaz_log!("rssweep w={next:.1} pane={pane} root={root} rehint={}", rehint as u8);
            // Done at the target, when the clamp stops all progress, or
            // past any reasonable drag: settle without persisting.
            if next == from || (self.resize.sweep.as_ref().is_some_and(|sweep| next == sweep.target)) || ticks > 10_000
            {
                let done = self.resize.width;
                self.resize.sweep = None;
                self.resize.active = false;
                self.resize.scripted = false;
                crate::baaz_log!("rssweep done w={done:.1}");
            } else {
                cx.notify();
            }
        }
        // A frame-paced sidebar-wheel sweep (`sidebar-scroll-sweep:` step):
        // the same push the real capture handler
        // makes (`sidebar_view::sidebar_wheel_capture`), once per rendered
        // frame instead of once per posted `CGEvent` — the in-process
        // fallback where the environment delivers no real gesture (see
        // `docs/02-app.md`). Re-arms itself via `request_animation_frame`
        // through `push_sidebar_scroll_sweep`'s own notify; drops itself
        // when the sweep reports done.
        if self.sidebar_scroll_sweep.is_some() {
            let dy = self.sidebar_scroll_sweep.as_mut().and_then(|sweep| sweep.advance());
            match dy {
                Some(dy) => {
                    self.push_sidebar_scroll_sweep(dy, cx);
                    window.request_animation_frame();
                }
                None => self.sidebar_scroll_sweep = None,
            }
        }
        // A frame-paced transcript-wheel sweep (`transcript-scroll-sweep:`
        // step): the transcript's twin of the
        // sidebar sweep above, pushing into the active session's own
        // accumulator (`SessionView::push_wheel`) instead of the sidebar's.
        if self.transcript_scroll_sweep.is_some() {
            let dy = self.transcript_scroll_sweep.as_mut().and_then(|sweep| sweep.advance());
            match dy {
                Some(dy) => {
                    if let Some(view) = self.active.clone() {
                        view.update(cx, |view, cx| {
                            view.push_wheel(px(dy));
                            cx.notify();
                        });
                    }
                    window.request_animation_frame();
                }
                None => self.transcript_scroll_sweep = None,
            }
        }
        // The sidebar pane's inputs may have changed without a notify of its
        // own (a session event, a probe answer, the minute rollover): re-arm
        // it here, before anything draws, so a transcript notify alone never
        // rebuilds the column.
        self.sync_sidebar_pane(cx);
        // The frame trace's per-tick row: written
        // here, not from `render_transcript`, because this runs once per
        // `Harness::render` regardless of which subtree actually rebuilt —
        // the true per-display-tick hook, where the old centre-scoped trace
        // went silent through a sidebar-only or resize-only gesture (parts
        // 4/C1/C2 stopped those from touching the cached transcript at
        // all). Gated up front so a disabled build pays nothing beyond the
        // one flag check; `sidebar_list`/`resize.width` are read straight
        // off `self` so nothing about the sidebar needs to render for this
        // tick to trace.
        // `take_trace_rehint` is a single flag shared with the
        // `resize-sweep:` step's own log above: on a tick where both a
        // sweep and the trace are running, the sweep's earlier drain wins
        // and this row reads `rehint=0` regardless. Irrelevant to the real
        // CGEvent-driven measurements this instrument exists for (they
        // never run a scripted sweep at the same time).
        if session::frame_trace_enabled() {
            let top = self.sidebar_list.logical_scroll_top();
            let (transcript, rehint) = self
                .active
                .clone()
                .map(|view| view.update(cx, |view, _| (Some(view.trace_tick()), view.take_trace_rehint())))
                .unwrap_or((None, false));
            session::note_root_frame_trace(
                top.item_ix,
                f32::from(top.offset_in_item),
                self.resize.width,
                self.resize.active,
                transcript,
                self.sidebar_gesture_active(),
                rehint,
            );
        }
        // The connect screen owns the whole window while it is up: it
        // follows the status cache and runs parked row actions, nothing
        // else. The login screen owns it the same way, but only for the
        // offline/replay captures that still boot into it — a live launch
        // never gates on Muse any more, so a signed-out shell runs the
        // lifecycle below like a signed-in one.
        if self.show_connect {
            self.sync_provider_state(cx);
            self.run_pending_connect_actions(window, cx);
            return;
        }
        if !matches!(self.auth, Auth::SignedIn(_))
            && (self.args.offline || self.args.replay.is_some())
        {
            return;
        }
        self.sync_provider_state(cx);
        self.run_pending_connect_actions(window, cx);
        // The right pane's git and filesystem reads, reconciled off the render
        // path: at once on open, kind or project change, on the 2 s interval
        // while open on a data kind, never while closed or on Browser.
        self.sync_right_cache(cx);
        // The scripted boot, every frame until it fires: the first session
        // for `--session`/`--send`/`--steps` opens without waiting for
        // `session/list`, then the script runs once the session is open.
        // Both are idempotent (consumed args, drained list), so the
        // per-frame call is a gate, not a loop.
        self.ensure_boot_session(window, cx);
        self.maybe_run_steps(window, cx);
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
        // The whole-frame instrument's start; the trailing marker below
        // closes it after paint (see `session::draw_end_marker`).
        session::note_draw_start();
        crate::sidebar_view::note_baaz_render();
        self.on_frame(window, cx);
        // The login screen owns the whole window; the shell is not built behind
        // it, so nothing of the signed-in state can leak into a capture.
        // The connect screen owns it the same way on a first run. Any other
        // launch renders the shell at once: a slow or signed-out Muse never
        // gates the window (offline/replay captures still boot into the
        // login screen for their sample states).
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let showing_login =
            !signed_in && (self.args.offline || self.args.replay.is_some()) && !self.show_connect;
        let body: AnyElement = if self.show_connect {
            self.render_connect(window, cx).into_any_element()
        } else if !showing_login {
            // The column is its own cached view: clean,
            // gpui reuses its retained subtree and only the centre rebuilds.
            // `size_full` is what the column wears itself (`render_sidebar`
            // fills its cell), so the cached layout resolves to the same
            // bounds the shell offers and any resize re-renders through the
            // bounds key.
            // No `.cached(...)` while assistive tech is on — see `render_centre`.
            let sidebar: AnyElement = if window.is_a11y_active() {
                self.sidebar_pane.clone().into_any_element()
            } else {
                self.sidebar_pane.clone().cached(StyleRefinement::default().size_full()).into_any_element()
            };
            let centre = self.render_centre(window, cx);
            // Painted lights off: the window owns real, glossy ones, and
            // the painted set only ever stacked underneath them.
            let kind = layout::right_kind(&self.layout);
            // Drawn from the cache the refresh path maintains: building this
            // element performs no subprocess or filesystem I/O of its own.
            // Deliberately not `.cached(...)` like the sidebar and transcript
            // columns (see the report): the pane's rows carry live actions and
            // hover state, and a retained subtree would need its own
            // key/invalidation to avoid going stale — while the subtree
            // rebuild itself is cheap once the reads are gone.
            let right_project = self.right_project();
            // Z7a2: the Browser kind draws the active session's live
            // webview (created lazily here, where the window is at
            // hand), every other kind draws from the read cache as
            // before. Gated on pane-open-on-Browser: activation, boot
            // and every other kind never create a webview as a render
            // side effect — creation happens only where the pane shows.
            let browser = (self.layout.right_open && kind == layout::RightKind::Browser)
                .then(|| self.ensure_browser_person(window, cx));
            let right = right::render(kind, &self.right_cache, right_project, browser.as_ref(), cx);
            let shell = app_shell("shell")
                .sidebar_width(px(self.resize.width))
                .right_width(px(self.right_resize.width))
                .resizing(self.resize.active || self.right_resize.active || std::mem::take(&mut self.right_snap))
                .traffic_lights(false)
                .sidebar_open(self.sidebar_open)
                // The header row stands still while the pane collapses: the
                // sidebar cell keeps its width — and the native-lights
                // reservation — so the toggle and search stay where they are.
                .header_follows_sidebar(false)
                .right_open(self.layout.right_open)
                .header_sidebar(
                    sidebar_header("hd-side")
                        .native_lights(true)
                        .on_toggle_sidebar(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
                        .on_search(cx.listener(|this, _, window, cx| this.open_search(window, cx))),
                )
                .header_centre(self.render_centre_header(window, cx))
                .header_right(
                    header_cell("hd-right").child(
                        div()
                            .id("hd-right-title")
                            .role(gpui::Role::Label)
                            .aria_label(kind.label())
                            .child(kind.label()),
                    ),
                )
                .sidebar(sidebar)
                .rail(self.render_rail(cx))
                .right(right)
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
            // The right strip sits over the right divider, centred on the
            // settled edge from the RIGHT: the pane fills the window's right
            // edge, so the divider stands `width` left of it. Hidden with
            // the pane: there is no divider to grab while it stands closed.
            if self.layout.right_open {
                let press = cx.entity().downgrade();
                let travel = cx.entity().downgrade();
                let release = cx.entity().downgrade();
                stack = stack.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .right(px(self.right_resize.width - RESIZE_HANDLE_W / 2.0))
                        .id("right-resize-area")
                        .role(gpui::Role::Splitter)
                        .aria_label("Resize right pane")
                        .child(
                            resize_handle("right-resize")
                                .on_drag_start(move |x, _, cx| {
                                    press.update(cx, |this, cx| this.begin_right_resize(x, cx)).ok();
                                })
                                .on_drag(move |x, _, cx| {
                                    travel.update(cx, |this, cx| this.drag_right_resize(x, cx)).ok();
                                })
                                .on_drag_end(move |_, cx| {
                                    release.update(cx, |this, cx| this.end_right_resize(cx)).ok();
                                }),
                        ),
                );
            }
            stack.into_any_element()
        } else {
            self.render_login(window, cx).into_any_element()
        };
        let dialog = self.render_dialog(window, cx);
        let settings = self.render_settings(cx);
        let muse_sheet = self.render_muse_sheet(window, cx);
        let palette = self.render_palette(cx);
        let toasts = self.render_toasts(cx);
        // Mid-drag the overlay covers the window, so the drag survives the
        // pointer outrunning the 6 px strip; moves alone would go silent.
        // One overlay, not two: the two drags never run together, so it
        // routes every move to whichever one is in flight.
        let capture: Option<AnyElement> =
            (self.resize.active || self.right_resize.active).then(|| {
                let travel = cx.entity().downgrade();
                let release = cx.entity().downgrade();
                drag_capture_overlay("resize-capture")
                    .on_drag(move |x, _, cx| {
                        travel
                            .update(cx, |this, cx| {
                                if this.resize.active {
                                    this.drag_resize(x, cx);
                                } else {
                                    this.drag_right_resize(x, cx);
                                }
                            })
                            .ok();
                    })
                    .on_drag_end(move |_, cx| {
                        release
                            .update(cx, |this, cx| {
                                if this.resize.active {
                                    this.end_resize(cx);
                                } else {
                                    this.end_right_resize(cx);
                                }
                            })
                            .ok();
                    })
                    .into_any_element()
            });
        let overflow = self.render_overflow_menu(cx);
        let row_detail = self.render_row_detail(cx);
        let view_options = self.render_view_menu(cx);
        let account = self.render_account_menu(cx);
        let project_menu = self.render_project_menu(cx);
        // Z7a, the native-overlay rule: persist real navigations, resolve
        // pending screenshot attaches, and hide every native webview this
        // frame covers (the page is composited above gpui, so gpui cannot
        // draw over it — the host takes the view out instead).
        self.sync_browser(
            overflow.is_some(),
            view_options.is_some(),
            account.is_some(),
            project_menu.is_some(),
            window,
            cx,
        );
        // The palette takes the keyboard the frame it opens, so the arrows and
        // the return reach it rather than the composer under it. The search
        // palette is the exception: its query field owns the keyboard, and the
        // arrows and the return reach the list through the overlay's own menu
        // context. The Projects and Commands palettes are the same, each with
        // its own field.
        let searching = self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Search);
        let projecting =
            self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Projects);
        let commanding =
            self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Commands);
        if searching {
            let query = self.search_query.focus_handle(cx);
            if !query.is_focused(window) {
                window.focus(&query, cx);
            }
        } else if projecting {
            let query = self.projects_query.focus_handle(cx);
            if !query.is_focused(window) {
                window.focus(&query, cx);
            }
        } else if commanding {
            let query = self.commands_query.focus_handle(cx);
            if !query.is_focused(window) {
                window.focus(&query, cx);
            }
        } else if palette.is_some() && !self.focus_palette.is_focused(window) {
            window.focus(&self.focus_palette, cx);
        }
        // An overlay that closes unmounts the element it had focused, and the
        // window goes on reporting that handle as focused: `focused()` is
        // `Some` and `is_focused()` is true, while the element is no longer
        // in the rendered frame. gpui resolves a keystroke against the
        // focused node *in that frame* and, finding none, falls back to the
        // dispatch tree's root — which sits ABOVE this view's div. Every
        // `on_action` handler mounted here is skipped from then on.
        //
        // That is not limited to context-scoped bindings or to the palette's
        // own shortcut. Measured: one open-and-close of the ⌘K palette left
        // ⌘B dead too, because both handlers hang off the same div. It is
        // why "it works once, then it doesn't".
        //
        // So the trigger is the *transition* — an overlay was up last frame
        // and is gone now — rather than "nothing is focused", which is never
        // true here, or "nothing on screen wants the keyboard", which cannot
        // be told from the window. Parking focus on the root only on that
        // edge leaves the composer's own focus alone the rest of the time.
        let overlay_now = self.overlays.read(cx).palette.is_some()
            || self.overlays.read(cx).dialog.is_some()
            || self.overlays.read(cx).menu.is_some()
            || self.overlays.read(cx).settings.is_some();
        let overlay_just_closed = self.overlay_was_open && !overlay_now;
        // Two triggers, and both are needed. The first frame has nothing
        // focused at all, so without the `is_none` arm the very first
        // shortcut never fires either.
        if (window.focused(cx).is_none() || overlay_just_closed)
            && !self.terminal_focus.is_focused(window)
        {
            window.focus(&self.focus_root, cx);
        }
        self.overlay_was_open = overlay_now;
        aui::keys::track_pointer(
            div()
                .size_full()
                .relative()
                .key_context(aui::keys::ROOT_CONTEXT)
                .track_focus(&self.focus_root)
                // A mouse-down outside the browser page's rect hands the
                // keyboard back when the page holds it. Capture phase, next
                // to `track_pointer`, and never consuming: the click still
                // reaches its target.
                .capture_any_mouse_down(cx.listener(
                    |this, event: &gpui::MouseDownEvent, window, cx| {
                        this.release_browser_keyboard_on_mouse_down(event, window, cx);
                    },
                ))
                .on_action(cx.listener(|this, _: &OpenModelMenu, _, cx| this.open_picker(MenuKind::Model, cx)))
                .on_action(cx.listener(|this, _: &OpenEffortMenu, _, cx| this.open_picker(MenuKind::Effort, cx)))
                .on_action(cx.listener(|this, _: &OpenModeMenu, _, cx| this.open_picker(MenuKind::Mode, cx)))
                .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
                .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_session(window, cx)))
                .on_action(cx.listener(|this, _: &AddProject, window, cx| this.open_projects(false, window, cx)))
                .on_action(cx.listener(|this, _: &Interrupt, _, cx| this.interrupt(cx)))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.open_search(window, cx)))
                .on_action(cx.listener(|_, _: &MinimizeWindow, window, _| window.minimize_window()))
                .on_action(cx.listener(|_, _: &ZoomWindow, window, _| window.zoom_window()))
                .on_action(cx.listener(|_, _: &ToggleTheme, window, cx| AuiTheme::toggle_kind(Some(window), cx)))
                .on_action(cx.listener(|this, _: &ShowAbout, _, cx| this.show_about(cx)))
                .on_action(cx.listener(|this, _: &OpenSettings, _, cx| this.open_settings(0, cx)))
                .on_action(cx.listener(|this, _: &ShowDocs, _, _| this.show_docs()))
                .on_action(cx.listener(|this, _: &aui::keys::TogglePalette, _, cx| {
                    this.open_palette(PaletteKind::Commands, cx)
                }))
                // `aui::keys` binds ⌘\ to *its own* `ToggleRightPane`, a
                // different action type from baaz's same-named one (bound
                // to ⌘⌥B and handled on the centre below) — two crates,
                // one name. Nothing listened for the library's, so ⌘\
                // dispatched into the void. Handle it here, on the root,
                // next to the library's other actions.
                .on_action(cx.listener(|this, _: &aui::keys::ToggleRightPane, _, cx| {
                    this.toggle_right(cx);
                }))
                // ⌘L at window level: the webview's own `cmd-l` binding
                // only fires while gpui holds the keyboard, so this root
                // binding carries it when the native page has focus. The
                // handler itself routes to the browser only while the
                // right pane shows it.
                .on_action(cx.listener(|this, _: &aui_webview::FocusAddress, window, cx| {
                    this.focus_browser_address(window, cx);
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
                .children(settings)
                .children(muse_sheet)
                .children(row_detail)
                .children(overflow)
                .children(view_options)
                .children(account)
                .children(project_menu)
                // Painted last: the whole-frame instrument's end. Zero-size,
                // paints nothing, takes no space — captures are unaffected.
                .child(session::draw_end_marker()),
        )
    }
}

/// Where Help → Baaz Documentation looks for the docs folder: beside the
/// working directory first, then three ancestors above the executable
/// (`target/debug/baaz` is three levels below the repo root: the exe
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

    /// Help → Baaz Documentation: the docs folder in Finder.
    ///
    /// The docs live beside the repo, so this looks for them next to the
    /// working directory first (`cargo run` from the repo root) and then
    /// three ancestors above the executable (`target/debug/baaz` is
    /// three levels below the root). A bundled app moved away from the
    /// repo has no docs beside it, and that is an `eprintln`, not a dialog.
    fn show_docs(&mut self) {
        match docs_dir() {
            Some(dir) => {
                if std::process::Command::new("open").arg(dir).spawn().is_err() {
                    crate::baaz_log!("could not reveal {}", dir.display());
                }
            }
            None => crate::baaz_log!("no docs folder beside the app"),
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
        // The Skills page is the next thing Escape takes back, from any
        // focus the window's own Cancel reaches (V1): the page's own
        // `SkillsClose` only fires with focus inside the page, so from the
        // dock or the sidebar nothing left it. One layer per press, through
        // the same `close_skills` the page's key takes — a dialog, then the
        // menu, then the page. A text field that consumes Escape never
        // reaches here, so field editing is untouched.
        if self.skills.open {
            self.close_skills(cx);
            return;
        }
        // The composer's `+` menu is view-local, so the overlay stack
        // above never saw it: Escape closes it here, before edits (V1).
        if let Some(view) = self.active.clone() {
            if view.update(cx, |view, cx| view.close_plus_menu(cx)) {
                return;
            }
        }
        // An open file preview is the next thing Escape takes back: back
        // to the tree, keeping the selected marker and the scroll offset.
        if let Some((root, _)) = self.right_project() {
            if self.close_file_preview_for(&root, cx) {
                return;
            }
        }
        // An open rename is the next thing Escape takes back — a project
        // rename first, then a session row's. (There is no sidebar search
        // field left to clear: ⌘⇧F owns search now, and its palette closes
        // through the overlay stack above.)
        if self.renaming_project.take().is_some() {
            self.focus_composer = true;
            cx.notify();
            return;
        }
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
    use super::Harness;
    // `cx.new` is `AppContext`'s, and the trait has to be in scope for it.
    use gpui::AppContext as _;
    use std::path::PathBuf;

    /// A bootable [`crate::Args`] pointed at a hermetic state dir.
    fn test_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        }
    }

    /// Point `BAAZ_STATE_DIR` at a fresh temp dir for the test's duration,
    /// restoring whatever was there before. Returns the lock guard, the old
    /// value and the dir; the caller restores and drops them at the end.
    fn hermetic_state(name: &str) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-right-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    /// Undo [`hermetic_state`]: remove the temp dir, put the old value back,
    /// release the lock so no test leaks its dir into another.
    #[allow(clippy::needless_pass_by_value)]
    fn restore_state(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf),
    ) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    /// K1: a root-context shortcut's precondition survives an overlay
    /// closing. The palette opens and closes over the model, then one draw
    /// lets the frame's focus logic run: the open palette renders through
    /// a deferred layer, and drawing that layer across two test draws
    /// trips gpui's stale-arena panic, so the test draws once, after the
    /// close — which is also the state the assertion is about. Adjusted
    /// from the report's sketch, which never drew the view and so could
    /// hold no focus either way.
    #[gpui::test]
    fn a_root_shortcut_still_fires_after_an_overlay_closes(cx: &mut gpui::TestAppContext) {
        use crate::overlays::PaletteKind;
        use gpui::prelude::*;
        let state = hermetic_state("shortcut-repro");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(vc.update(|_, cx| baaz.read(cx).layout.right_open));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_palette(PaletteKind::Commands, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            h.overlays.update(cx, |o, _| o.palette = None);
        }));
        vc.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(1440.), gpui::px(900.)),
            |_, _| baaz.clone().into_any_element(),
        );
        let focused = vc.update(|window, cx| window.focused(cx).is_some());
        assert!(focused, "nothing holds focus after the overlay closed, so every root-context binding is dead");
        restore_state(state);
    }

    /// T3b: the dock hint names the agent that owns the running tab — or
    /// no agent when none does. A Codex bridge id (the pre-ack request id
    /// the durable record never names) resolves through the bridge map;
    /// a minted id resolves through the durable record; an unknown tab
    /// goes neutral rather than wearing Muse's name. Remove either map
    /// and its arm reads wrong here.
    #[gpui::test]
    fn dock_hint_names_the_tab_owner_or_no_agent(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let state = hermetic_state("dock-hint");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        let root = state.2.clone();
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                // The durable record names a minted session …
                h.provider_sessions.insert(
                    "minted-1".to_owned(),
                    crate::provider_sessions::ProviderSessionRecord {
                        provider: "codex".to_owned(),
                        session_id: "minted-1".to_owned(),
                        workspace: None,
                        project: None,
                        created_ms: 0,
                        updated_ms: 0,
                        turns: 0,
                        title: None,
                        first_prompt: None,
                        handoff_to: None,
                        handoff_from: None,
                        handoff_from_provider: None,
            handoff_title: None,
                        display_texts: std::collections::HashMap::new(),
                    },
                );
                // … while the bridge map names the pre-ack request id.
                h.register_terminal_session("cmd-open", root.clone(), "codex");
                h.register_terminal_session("cc-bridge", root.clone(), "claude-code");
                h.register_terminal_session("m-bridge", root, "muse");
            });
        });
        vc.update(|_, cx| {
            let hint = |h: &Harness, origin: Option<&str>| {
                let provider = h.terminal_tab_provider(origin);
                h.terminal_hint_name(provider)
            };
            let h = baaz.read(cx);
            assert_eq!(hint(h, Some("minted-1")), "Codex can type here");
            assert_eq!(hint(h, Some("cmd-open")), "Codex can type here");
            assert_eq!(hint(h, Some("cc-bridge")), "Claude Code can type here");
            assert_eq!(hint(h, Some("m-bridge")), "Muse can type here");
            assert_eq!(hint(h, None), "The agent can type here");
            assert_eq!(hint(h, Some("no-such-tab")), "The agent can type here");
        });
        restore_state(state);
    }

    /// Z7a2: open a local session view the way the scripted chrome does —
    /// what the browser tests activate without a provider child.
    fn open_test_session(
        vc: &mut gpui::VisualTestContext,
        baaz: &gpui::Entity<Harness>,
        workspace: &std::path::Path,
    ) {
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let host = crate::session::SessionHost {
                    provider_id: "echo".to_owned(),
                    workspace: workspace.to_string_lossy().into_owned(),
                    overlays: h.overlays.clone(),
                    capture: crate::shot::CaptureToken::default(),
                    terminal_host: None,
                };
                let view = cx.new(|cx| {
                    crate::session::SessionView::new("s-1".to_owned(), None, host, window, cx)
                });
                h.active = Some(view);
            })
        });
    }

    /// Z7a2: draw the signed-in shell once, the way the focus tests do.
    fn draw_shell(vc: &mut gpui::VisualTestContext, baaz: &gpui::Entity<Harness>) {
        use gpui::IntoElement as _;
        vc.draw(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(1440.), gpui::px(900.)),
            |_, _| baaz.clone().into_any_element(),
        );
    }

    /// Z7a2: no webview exists until the pane shows Browser — neither boot
    /// nor a session switch with no Browser state creates one.
    #[gpui::test]
    fn browser_webviews_wait_for_their_pane(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browser-lazy");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.states.is_empty()
                && baaz.read(cx).browser.home.is_none()),
            "boot draws no webview"
        );
        // A session switch with no Browser state: the activate tail —
        // focus the composer, restore the (absent) pane state.
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.focus_composer = true;
                h.restore_right_for_session("s-1", cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.states.is_empty()
                && baaz.read(cx).browser.home.is_none()),
            "a switch with no Browser state creates no webview"
        );
        restore_state(state);
    }

    /// Z7a2: a restore onto Browser creates the webview (lazy, on show)
    /// without moving focus off the composer.
    #[gpui::test]
    fn restoring_browser_keeps_the_composers_focus(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browser-restore-focus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.set_override(
                    "s-1",
                    |meta| {
                        meta.right = Some(crate::sessions::RightState {
                            open: true,
                            kind: crate::layout::RightKind::Browser,
                            ..Default::default()
                        });
                    },
                    cx,
                );
                // What `activate` sets before the restore below.
                h.focus_composer = true;
                h.restore_right_for_session("s-1", cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        let url_focus = vc.update(|_, cx| {
            baaz.read(cx)
                .browser
                .states
                .get("s-1")
                .cloned()
                .expect("restore onto Browser creates the webview")
                .read(cx)
                .focus_handle()
                .clone()
        });
        let composer_focus = vc.update(|_, cx| {
            baaz.read(cx)
                .active
                .clone()
                .expect("a session is open")
                .update(cx, |view, cx| view.composer_focus_handle(cx))
        });
        let focused = vc.update(|window, cx| window.focused(cx));
        assert_eq!(
            focused,
            Some(composer_focus),
            "a restored Browser pane leaves focus where activation put it"
        );
        assert_ne!(focused, Some(url_focus), "restore must not steal into the URL field");
        restore_state(state);
    }

    /// Z7a2: the person opening Browser on a blank page lands in the URL
    /// field — the one focus move the pane is allowed.
    #[gpui::test]
    fn opening_browser_by_hand_focuses_the_url(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browser-person-focus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        draw_shell(&mut *vc, &baaz);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Browser, cx)));
        draw_shell(&mut *vc, &baaz);
        let url_focus = vc.update(|_, cx| {
            baaz.read(cx)
                .browser
                .states
                .get("s-1")
                .cloned()
                .expect("opening Browser creates the webview")
                .read(cx)
                .focus_handle()
                .clone()
        });
        let focused = vc.update(|window, cx| window.focused(cx));
        assert_eq!(
            focused,
            Some(url_focus),
            "the person's own open onto a blank page focuses the URL field"
        );
        restore_state(state);
    }

    /// `toggle_right` twice returns the pane to its start.
    #[gpui::test]
    fn toggling_the_right_pane_twice_returns_to_its_start(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("toggle");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        let start = vc.update(|_, cx| baaz.read(cx).layout.right_open);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).layout.right_open),
            !start,
            "one toggle flips the pane"
        );
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).layout.right_open),
            start,
            "two toggles return to the start"
        );
        restore_state(state);
    }

    /// P2: the hero carries no provider control — the picker lives in the
    /// composer's action row now (the hero switcher was an accident of an
    /// underspecified brief, never the design). Draws both empty states (a
    /// project with no session open, and no project at all) and asserts the
    /// picker left no measured bounds in either.
    ///
    /// The absence assert reads `debug_bounds("provider-picker")`, which only
    /// ever holds entries for elements with an explicit `.debug_selector()`.
    /// The deleted picker had one, so a reintroduction in this file's house
    /// style would trip it. The hero buttons cannot serve as the "something
    /// drew" guard the same way: aui's `button()` sets an element id, which
    /// gpui does not record bounds for, so probing either hero button id
    /// always misses even when the hero draws perfectly. The guard below
    /// instead pins the live state to the signed-in empty state (where
    /// `render` deterministically takes the `render_no_session` branch),
    /// proves a frame painted, and proves the shell's own frame logic ran
    /// against that state via the window title — a run that rendered nothing
    /// at all fails here rather than passing the picker assert vacuously.
    #[gpui::test]
    fn hero_has_no_provider_picker(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("hero-no-provider-picker");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        for no_project in [false, true] {
            let (baaz, vc) = cx.add_window_view(|window, cx| {
                let mut args = test_args(&state.2);
                args.no_project = no_project;
                Harness::new(args, crate::shot::CaptureToken::default(), window, cx)
            });
            // The boot lands on the login screen; the hero only renders in
            // the signed-in shell, so step past it with the sample identity
            // (no child runs behind it — the chrome draws, the wire idles).
            vc.update(|window, cx| {
                baaz.update(cx, |h, cx| {
                    h.args.login = crate::LoginSample::SignedIn;
                    h.apply_login_sample(window, cx);
                })
            });
            vc.run_until_parked();
            // A resize forces a draw; without one no frame settles and the
            // bounds below would miss the picker trivially.
            vc.simulate_resize(gpui::size(gpui::px(900.), gpui::px(800.)));
            vc.run_until_parked();
            // Which hero variant drew is decided by the live project state:
            // `--no-project` boots with nothing adopted (`hero-new`), while
            // the explicit workspace is adopted at boot (`new-session`).
            // Pin the mapping so both arms provably cover their variant.
            let (signed_in, no_session, has_project, expected_title) = vc.update(|_, cx| {
                let h = baaz.read(cx);
                (matches!(h.auth, super::Auth::SignedIn(_)), h.active.is_none(), h.current_project().is_some(), h.window_title(cx))
            });
            assert!(signed_in, "the shell never signed in (no_project={no_project})");
            assert!(no_session, "a session opened on its own (no_project={no_project})");
            assert_eq!(
                has_project, !no_project,
                "expected the {no_project} arm to land on the other hero variant"
            );
            // A frame painted since the resize: an empty quad list means the
            // window never drew and the picker assert below would be vacuous.
            let painted = vc.update(|window, _| window.painted_quads().len());
            assert!(painted > 0, "the window painted nothing (no_project={no_project})");
            // The shell's own frame logic ran against the live state: the
            // title is only written from `on_frame`, so a matching title
            // proves a frame settled after the boot above.
            assert_eq!(
                vc.window_title().as_deref(),
                Some(expected_title.as_str()),
                "no frame settled on the signed-in empty state (no_project={no_project})"
            );
            assert!(
                vc.debug_bounds("provider-picker").is_none(),
                "the hero still draws a provider picker (no_project={no_project})"
            );
        }
        restore_state(state);
    }

    /// `show_right` opens each of the four kinds, closes the pane when
    /// called with the kind already showing, and switches kinds without
    /// closing — all with no session open.
    #[gpui::test]
    fn show_right_opens_each_kind_and_closes_the_current_one(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("show");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "no session is open");
        for kind in crate::layout::RightKind::ALL {
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(kind, cx)));
            assert!(
                vc.update(|_, cx| baaz.read(cx).layout.right_open),
                "{kind:?} opens the pane"
            );
            assert_eq!(vc.update(|_, cx| baaz.read(cx).layout.right_kind), Some(kind));
            // The same kind again closes the pane, keeping the kind so a
            // later reopen restores it.
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(kind, cx)));
            assert!(
                !vc.update(|_, cx| baaz.read(cx).layout.right_open),
                "{kind:?} again closes the pane"
            );
            assert_eq!(vc.update(|_, cx| baaz.read(cx).layout.right_kind), Some(kind));
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "show_right never opens a session");
        }
        // A different kind while open switches without closing.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Diff, cx))
        });
        assert!(vc.update(|_, cx| baaz.read(cx).layout.right_open));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).layout.right_kind),
            Some(crate::layout::RightKind::Diff)
        );
        restore_state(state);
    }

    /// Z2: the right pane belongs to each session and a switch restores it
    /// without animating. A shows Diff while B shows Browser; every switch
    /// restores the shown session's pane (B first shows it closed); a
    /// restart restores from the store; the restore arms `right_snap` for
    /// exactly one frame while user toggles never arm it; a new session
    /// starts closed; Files previews are per session too.
    #[gpui::test]
    fn the_right_pane_is_per_session_and_restores_without_animating(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use gpui::prelude::*;
        let state = hermetic_state("right-per-session");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        // The shell (and its snap-consuming frame) only renders signed in.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        std::fs::write(state.2.join("Cargo.toml"), "[package]\n").expect("preview fixture");
        // Small helpers as macros: closures would all borrow `vc` mutably
        // and refuse to coexist.
        macro_rules! open_session {
            ($id:expr) => {
                vc.update(|window, cx| baaz.update(cx, |h, cx| h.resume($id.to_owned(), window, cx)))
            };
        }
        macro_rules! show {
            ($kind:expr) => {
                vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right($kind, cx)))
            };
        }
        macro_rules! pane {
            () => {
                vc.update(|_, cx| {
                    let h = baaz.read(cx);
                    (h.layout.right_open, h.layout.right_kind, h.right_snap)
                })
            };
        }
        macro_rules! draw {
            () => {
                vc.draw(
                    gpui::point(gpui::px(0.), gpui::px(0.)),
                    gpui::size(gpui::px(1440.), gpui::px(900.)),
                    |_, _| baaz.clone().into_any_element(),
                )
            };
        }
        // Session A shows Diff; a user change never arms the snap.
        open_session!("sess-a");
        show!(crate::layout::RightKind::Diff);
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Diff), false));
        // Switching to B (no stored state) closes the pane, snapped — and
        // one frame consumes the snap.
        open_session!("sess-b");
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Diff), true));
        draw!();
        assert!(!vc.update(|_, cx| baaz.read(cx).right_snap), "one frame consumes the snap");
        // Browser in B; a user change still does not snap.
        show!(crate::layout::RightKind::Browser);
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Browser), false));
        // Back to A: Diff again — open throughout, so no snap. Back to B:
        // Browser again, no snap either.
        open_session!("sess-a");
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Diff), false));
        open_session!("sess-b");
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Browser), false));
        draw!();
        // Closed to open snaps too: close in B, then return to A.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Browser), false));
        open_session!("sess-a");
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Diff), true));
        draw!();
        assert!(!vc.update(|_, cx| baaz.read(cx).right_snap), "one frame consumes the snap");
        // A user toggle after the snap is consumed never re-arms it.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(!vc.update(|_, cx| baaz.read(cx).right_snap), "a user toggle must still animate");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Diff), false));
        // A restore arms the snap; a user toggle BEFORE any frame consumes it
        // must still animate (review finding): the toggle disarms it.
        open_session!("sess-b");
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Browser), true));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(!vc.update(|_, cx| baaz.read(cx).right_snap), "a toggle before the frame still animates");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        open_session!("sess-a");
        draw!();
        // Restart: the store carries A home as Diff.
        let disk = crate::sessions::read();
        assert_eq!(
            disk.get("sess-a").and_then(|meta| meta.right.clone()),
            Some(crate::sessions::RightState {
                open: true,
                kind: crate::layout::RightKind::Diff,
                files_preview: None,
                files_selected: None,
                files_expanded: Vec::new(),
                browser_url: None,
            })
        );
        let baaz2 = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz2.update(cx, |h, cx| h.resume("sess-a".to_owned(), window, cx)));
        assert!(vc.update(|_, cx| baaz2.read(cx).layout.right_open), "A restores Diff after restart");
        assert_eq!(
            vc.update(|_, cx| baaz2.read(cx).layout.right_kind),
            Some(crate::layout::RightKind::Diff)
        );
        // A brand-new session starts closed.
        vc.update(|window, cx| baaz2.update(cx, |h, cx| h.resume("sess-c".to_owned(), window, cx)));
        assert!(!vc.update(|_, cx| baaz2.read(cx).layout.right_open), "a new session starts closed");
        // Files previews are per session: A previews Cargo.toml, B nothing.
        open_session!("sess-a");
        show!(crate::layout::RightKind::Files);
        let root =
            vc.update(|_, cx| baaz.read(cx).right_project().map(|(root, _)| root).expect("a project"));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.begin_file_preview_for(&root, "Cargo.toml", cx)));
        macro_rules! previewed {
            () => {
                vc.update(|_, cx| {
                    baaz.read(cx).right_cache.preview_for(&root).map(|preview| preview.path.clone())
                })
            };
        }
        assert_eq!(previewed!().as_deref(), Some("Cargo.toml"));
        open_session!("sess-b");
        assert_eq!(previewed!(), None, "B previews nothing");
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Browser), true));
        open_session!("sess-a");
        assert_eq!(previewed!().as_deref(), Some("Cargo.toml"), "A previews Cargo.toml again");
        restore_state(state);
    }

    /// R1: the render path performs no I/O. Rendering every kind twice moves
    /// neither the git nor the walk counter; the explicit read path moves it,
    /// which is what proves the counter is wired to the reads and not dead.
    /// This is the test that would have caught the defect: the old `render`
    /// ran up to six git subprocesses and a directory walk per frame.
    #[gpui::test]
    fn right_render_performs_no_io(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("right-pure");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        let dir = std::env::temp_dir().join(format!("baaz-right-pure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("purity probe dir");
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let project = Some((dir.clone(), "pure".to_string()));
        crate::right::reset_io_count();
        for kind in crate::layout::RightKind::ALL {
            for _ in 0..2 {
                vc.update(|_, cx| {
                    baaz.update(cx, |harness, cx| {
                        let _ = crate::right::render(kind, &harness.right_cache, project.clone(), None, cx);
                    });
                });
            }
        }
        assert_eq!(
            crate::right::io_count(),
            0,
            "rendering the pane must not touch git or the filesystem"
        );
        let snapshot = crate::right::read_snapshot(&dir);
        assert!(
            crate::right::io_count() > 0,
            "the read path must move the counter, or the purity assert above is vacuous"
        );
        assert_eq!(snapshot.root, dir);
        let _ = std::fs::remove_dir_all(&dir);
        restore_state(state);
    }

    /// R1: the file tree's Refresh action performs a real re-read. Opening the
    /// pane on Files requests one through `show_right`, and `refresh_files`
    /// — what the pane's Refresh action calls — requests another; both land
    /// the listing in the cache after the background task drains.
    #[gpui::test]
    fn right_refresh_fills_the_cache_off_the_render_path(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("right-refresh");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.run_until_parked();
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project().map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        let files = vc.update(|_, cx| baaz.read(cx).right_cache.files_for(&root));
        assert!(files.is_some(), "the background re-read lands the file listing in the cache");
        crate::right::reset_io_count();
        // Outside any `Harness` update, exactly like the pane's own action
        // dispatch: nesting an entity update inside one panics.
        vc.update(|_, cx| {
            crate::right::refresh_files(baaz.downgrade(), cx);
        });
        vc.run_until_parked();
        assert!(
            crate::right::io_count() > 0,
            "Refresh re-reads the filesystem instead of toasting that it is not wired"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).right_cache.files_for(&root)).is_some(),
            "the Refresh re-read lands in the cache too"
        );
        restore_state(state);
    }

    /// H1: an open preview follows the file. Rewriting the previewed file on
    /// disk and running the pane's refresh lands the new bytes — the same
    /// background path the 2 s poll takes.
    #[gpui::test]
    fn right_refresh_reloads_a_previewed_file_rewritten_on_disk(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("right-preview-follows");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.run_until_parked();
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project().map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        std::fs::write(root.join("note.txt"), "version one\n").unwrap();
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.begin_file_preview_for(&root, "note.txt", cx)));
        vc.run_until_parked();
        let shows = |vc: &mut gpui::VisualTestContext, baaz: &gpui::Entity<Harness>| {
            vc.update(|_, cx| {
                baaz.read(cx)
                    .right_cache
                    .preview_for(&root)
                    .and_then(|preview| crate::right::preview_text_shown(&preview).map(str::to_string))
            })
        };
        assert!(
            shows(vc, &baaz).is_some_and(|code| code.contains("version one")),
            "the preview opens on the file's bytes"
        );
        // Rewritten longer, so the size stamp moves even on a coarse-mtime
        // filesystem; the next refresh lands the new bytes.
        std::fs::write(root.join("note.txt"), "version two, rewritten at length\n").unwrap();
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.refresh_right_now(cx)));
        vc.run_until_parked();
        assert!(
            shows(vc, &baaz).is_some_and(|code| code.contains("version two")),
            "the refresh reloads the rewritten file into the open preview"
        );
        restore_state(state);
    }

    /// Beginning one divider's drag while the other runs is a no-op: both
    /// drags are never active at once.
    #[gpui::test]
    fn the_two_resize_drags_never_run_together(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("drags");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.begin_resize(100.0, cx);
                assert!(h.resize.active);
                h.begin_right_resize(100.0, cx);
                assert!(!h.right_resize.active, "a right press mid-sidebar-drag is a no-op");
                h.end_resize(cx);
                assert!(!h.resize.active);
                h.begin_right_resize(100.0, cx);
                assert!(h.right_resize.active);
                h.begin_resize(100.0, cx);
                assert!(!h.resize.active, "a sidebar press mid-right-drag is a no-op");
                h.end_right_resize(cx);
                assert!(!h.right_resize.active);
            })
        });
        restore_state(state);
    }

    /// A scratch root with an optional `docs/` child, removed on drop.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "baaz-docs-test-{}-{}",
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
        let exe = exe_root.root.join("target").join("debug").join("baaz");
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

    /// The live action behind one `(keystroke, context)`: the last installed
    /// row wins the tie, so the tail is what the key fires.
    fn live_action_for(cx: &mut gpui::App, keystroke: &str, context: &str) -> Option<String> {
        let map = cx.key_bindings();
        let borrowed = map.borrow();
        let action = borrowed
            .bindings()
            .rfind(|binding| {
                let id = binding
                    .keystrokes()
                    .iter()
                    .map(|stroke| {
                        let modifiers = stroke.modifiers();
                        format!(
                            "{}{}{}{}{}",
                            if modifiers.control { "ctrl-" } else { "" },
                            if modifiers.alt { "alt-" } else { "" },
                            if modifiers.platform { "cmd-" } else { "" },
                            if modifiers.shift { "shift-" } else { "" },
                            stroke.key(),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let binding_context = binding.predicate().map(|predicate| predicate.to_string());
                id == keystroke && binding_context.as_deref() == Some(context)
            })
            .map(|binding| binding.action().name().rsplit("::").next().unwrap_or("").to_string());
        action
    }

    /// Point `BAAZ_STATE_DIR` at a fresh temp dir for a Shortcuts handler
    /// test, with the deterministic flag off so the user file reads. The
    /// caller restores both through `restore_state`'s return contract.
    fn shortcut_state(name: &str) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf) {
        let state = hermetic_state(name);
        let old_det = std::env::var_os("BAAZ_DETERMINISTIC");
        std::env::remove_var("BAAZ_DETERMINISTIC");
        if let Some(value) = old_det {
            std::env::set_var("BAAZ_SHORTCUTS_SAVED_DETERMINISTIC", value);
        } else {
            std::env::remove_var("BAAZ_SHORTCUTS_SAVED_DETERMINISTIC");
        }
        state
    }

    /// Undo [`shortcut_state`]: put the deterministic flag back, then the
    /// state dir and the lock through [`restore_state`].
    fn restore_shortcut_state(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf),
    ) {
        match std::env::var_os("BAAZ_SHORTCUTS_SAVED_DETERMINISTIC") {
            Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
            None => std::env::remove_var("BAAZ_DETERMINISTIC"),
        }
        std::env::remove_var("BAAZ_SHORTCUTS_SAVED_DETERMINISTIC");
        restore_state(state);
    }

    /// A [`Harness`] on a hermetic state dir, through the same boot the
    /// overlay tests use.
    fn shortcut_harness<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (&'a mut gpui::VisualTestContext, gpui::Entity<Harness>) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(dir), crate::shot::CaptureToken::default(), window, cx))
        });
        (vc, baaz)
    }

    /// X5: setting a binding through the handler path writes the temp keymap
    /// file, moves the effective binding, and reloads the live bindings so
    /// the new key fires without a restart.
    #[gpui::test]
    fn settings_shortcut_set_rebinds_through_the_file_and_reloads(cx: &mut gpui::TestAppContext) {
        let state = shortcut_state("shortcut-set");
        let (vc, baaz) = shortcut_harness(cx, &state.2);
        let id = gpui::SharedString::from(crate::settings::shortcut_row_id("NewSession", Some("AuiRoot")));
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Set("cmd-t".into()), cx);
            });
        });
        let file = std::fs::read_to_string(state.2.join("keymap.json")).expect("the handler writes the file");
        assert!(
            file.contains("cmd-t") && file.contains("NewSession"),
            "the file holds the rebind: {file}"
        );
        let current = crate::keymap::effective_binding("NewSession", Some("AuiRoot")).expect("rebound");
        assert_eq!(current.keystroke, "cmd-t", "the effective binding moved");
        assert!(
            vc.update(|_, cx| baaz.read(cx).shortcut_errors.is_empty()),
            "a good write hangs no error on the row"
        );
        assert_eq!(
            cx.update(|cx| live_action_for(cx, "cmd-t", "AuiRoot")),
            Some("NewSession".to_string()),
            "the reloaded bindings fire the new key at once"
        );
        restore_shortcut_state(state);
    }

    /// X5: a reserved keystroke is refused through the handler path with its
    /// reason on the row, and the file and the effective binding keep the old
    /// key.
    #[gpui::test]
    fn settings_shortcut_reserved_set_is_refused_with_its_reason(cx: &mut gpui::TestAppContext) {
        let state = shortcut_state("shortcut-reserved");
        let (vc, baaz) = shortcut_harness(cx, &state.2);
        let id = gpui::SharedString::from(crate::settings::shortcut_row_id("NewSession", Some("AuiRoot")));
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Set("cmd-q".into()), cx);
            });
        });
        let error = vc.update(|_, cx| baaz.read(cx).shortcut_errors.get(id.as_ref()).cloned());
        let error = error.expect("the refusal hangs on the row");
        assert!(
            error.contains("reserved") && error.contains("Quits the app"),
            "the reserved reason travels to the row: {error}"
        );
        let file = std::fs::read_to_string(state.2.join("keymap.json")).unwrap_or_default();
        assert!(!file.contains("cmd-q"), "the refused write lands nowhere: {file}");
        let current = crate::keymap::effective_binding("NewSession", Some("AuiRoot")).expect("still bound");
        assert_eq!(current.keystroke, "cmd-n", "the old binding stands");
        restore_shortcut_state(state);
    }

    /// X5: Clear through the handler path drops the rebind from the file and
    /// the default fires again; Record arms the row and Cancel disarms it.
    #[gpui::test]
    fn settings_shortcut_clear_restores_the_default(cx: &mut gpui::TestAppContext) {
        let state = shortcut_state("shortcut-clear");
        let (vc, baaz) = shortcut_harness(cx, &state.2);
        let id = gpui::SharedString::from(crate::settings::shortcut_row_id("NewSession", Some("AuiRoot")));
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Record, cx);
            });
        });
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).recording_shortcut.clone()),
            Some(id.to_string()),
            "Record arms the row"
        );
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Cancel, cx);
            });
        });
        assert!(
            vc.update(|_, cx| baaz.read(cx).recording_shortcut.clone()).is_none(),
            "Cancel disarms it"
        );
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Set("cmd-t".into()), cx);
            });
        });
        let rebound = crate::keymap::effective_binding("NewSession", Some("AuiRoot")).expect("rebound");
        assert_eq!(rebound.keystroke, "cmd-t");
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Clear, cx);
            });
        });
        let file = std::fs::read_to_string(state.2.join("keymap.json")).unwrap_or_default();
        assert!(!file.contains("cmd-t"), "Clear drops the rebind from the file: {file}");
        let current = crate::keymap::effective_binding("NewSession", Some("AuiRoot")).expect("still bound");
        assert_eq!(current.keystroke, "cmd-n", "Clear restores the default");
        restore_shortcut_state(state);
    }
}
