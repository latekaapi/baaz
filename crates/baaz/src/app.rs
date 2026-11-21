//! The application entity: the connection, auth, the session list and the shell.
//!
//! # Thread model
//!
//! Every session rides a provider lane ([`provider::Provider`]): Claude Code
//! and Codex a child each, Muse a lane of the one `muse serve` hub the window
//! connects ([`crate::conn::connect`], its slot in the window's registry,
//! [`Harness::providers`]). The
//! children run their own reader and writer threads, so no pipe touches the
//! UI thread. Two directions cross the boundary:
//!
//! * **Events in.** Each view drains its own lane's events on a foreground
//!   task, in the lane's order, so a frame always renders a consistent
//!   transcript. The hub's connection stream — every Muse session's row
//!   facts, sign-in, usage, the child's exit — is bridged onto one
//!   `futures` receiver the window drains (`on_hub_event`).
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
//! [`Harness`] keeps the fields, boot, connect, the hub's events and `render`, but most
//! of its concerns have their own modules and reach back in through one call
//! per seam:
//!
//! * [`crate::login`] — the `account/*` lane, the device and API-key flows
//!   (the `Login` entity the Muse sheet draws), and sign-out.
//! * [`crate::sidebar_view`] — the sidebar column as its own view
//!   (`SidebarPane`: nav block, session rows, the footer, the rail, Browse
//!   mode's rows), over the [`crate::session_list`] model; the root's half
//!   (the facts it pushes, the intents it maps, the hover card it seats) is
//!   `app/sidebar_host.rs`.
//! * [`crate::dialogs`] — what floats over the window: the modal, the
//!   palette, the toast stack and the header's overflow menu.
//! * [`crate::billing`] — the tier probe's lifecycle and the banner it hands
//!   to the open session.
//! * [`crate::resize`] — the sidebar and right-pane dividers' drags.
//! * [`crate::steps`] — `--steps` and `--login-steps`: the verb tables, the
//!   parser and the two runners.
//! * [`crate::wire`] — background call, then update.

use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(feature = "probe")]
use std::path::PathBuf;
use std::rc::Rc;

use aui::a11y_text::a11y_text_input;
use aui::composer::composer_state_rows;
use aui::data::{button, icon_button};
use aui::feedback::{banner, BannerKind, BannerRun};
use aui::keys::{Cancel, FocusNext, FocusPrev, TogglePalette, ToggleSidebar};
use aui::overlay::DialogKind;
use aui::shell::{RESIZE_HANDLE_W, app_shell, drag_capture_overlay, resize_handle};
use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui::{
    Pixels, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, actions, div, prelude::*, px, AnyElement, App, Context, Entity, ExternalPaths,
    FocusHandle, Focusable, SharedString, Subscription, Task, Window,
};
#[cfg(feature = "probe")]
use gpui::{point, PlatformInput, ScrollDelta, ScrollEvent};
use gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use aui::interact::{dismiss_layer, interactive, menu_panel, MENU_ROW_H, ROW_GAP};
use aui::util::{h_flex, v_flex};
use gpui_kit::component::Root;
use provider::ProviderEvent;

#[cfg(feature = "probe")]
use crate::auth::Identity;
use crate::conn;
use crate::index::{self, IndexEntry};
use crate::login::{Auth, Login};
use crate::overlays::{Dialog, DialogAction, MenuKind, Overlays, PaletteKind};
use crate::providers::ProviderId;
use crate::region::Region as _;
use crate::right;
use crate::runtime::CaptureToken;
use crate::session::{self, Draft, SessionEvent, SessionHost, SessionRuntime, SessionView};
use crate::tier::Tier;
use crate::sessions::SessionMeta;
use crate::projects::{self, Project, Projects};

use crate::sidebar::{self, SessionEntry};
use crate::sidebar_view::SidebarPane;
use crate::search::{FileHit, SessionHit};
use crate::terminal::{self, TerminalHost, TerminalService};
use crate::wire::WireCall;
use crate::{layout, Args};
use crate::layout::LayoutGlobal as _;

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
        /// Open the reasoning-effort picker (⌃⌘E; ⌘⇧E docks the agent column).
        OpenEffortMenu,
        /// Browse: open or close the ⌘J agent line (docs/27 §4.5).
        ToggleAgentLine,
        /// Browse: open or close the ⌘E agent card.
        ToggleAgentCard,
        /// Browse: dock the card as a column, or undock it (⌘⇧E).
        ToggleAgentColumn,
        /// Open the approval-mode picker (⌘⇧P).
        OpenModeMenu,
        /// Move the open menu's selection up.
        MenuUp,
        /// Move the open menu's selection down.
        MenuDown,
        /// ← in the open menu: the effort slider steps down; a model's
        /// submenu closes (D4).
        MenuLeft,
        /// → in the open menu: the effort slider steps up; a model's
        /// submenu opens (D4).
        MenuRight,
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
        /// Open the next session that needs the person (⌘⇧J, task 3.4).
        JumpNextWaiting,
        /// Toggle the right pane (⌘-backslash, from the library).
        ToggleRightPane,
        /// D6: commit from the commit message (⌘⏎).
        CommitMessage,
        /// D6: pop the pane's pushed view (Esc, ⌘[).
        RightBack,
        /// Show Changes in the right pane (⌘⇧D).
        ShowRightChanges,
        /// Show Files in the right pane (⌘⌥E).
        ShowRightFiles,
        /// Show Browser in the right pane (⌘⇧B).
        ShowRightBrowser,
        /// Switch between Sessions and Browse (⌥⌘B).
        ToggleBrowseMode,
        /// Open a blank browser tab (⌘T): a person tab in Browse, the
        /// shown owner's tab otherwise.
        NewBrowserTab,
        /// Close the active Browse tab (⌘W in Browse; in Sessions ⌘W
        /// keeps closing the window).
        CloseBrowseTab,
        /// Reopen the most recently closed tab (⌘⇧T), selecting it.
        ReopenBrowseTab,
        /// Select the n-th person tab on screen (⌘1–⌘8).
        BrowseTab1,
        BrowseTab2,
        BrowseTab3,
        BrowseTab4,
        BrowseTab5,
        BrowseTab6,
        BrowseTab7,
        BrowseTab8,
        /// Select the last person tab on screen (⌘9).
        BrowseTabLast,
        /// Move the active Browse tab down one slot (⌥⌘↓).
        MoveBrowseTabDown,
        /// Move the active Browse tab up one slot (⌥⌘↑).
        MoveBrowseTabUp,
        /// Open the find strip on the shown Browse tab (⌘F in Browse).
        FindInPage,
        /// Zoom the shown Browse page in one step (⌘=), kept for its site.
        ZoomIn,
        /// Zoom the shown Browse page out one step (⌘-).
        ZoomOut,
        /// The shown Browse page back to 100 % (⌘0).
        ZoomReset,
        /// The History page in Browse (⌘Y), or shut it.
        ShowHistory,
        /// The Bookmarks page in Browse (⇧⌘Y), or shut it.
        ShowBookmarks,
        BookmarkPage,
        ClosePeek,
        PeekToTab,
        /// Copy the shown Browse tab's link (⇧⌘C in Browse).
        CopyLink,
        /// Open the shown Browse tab's developer tools (⌥⌘I in Browse).
        DevTools,
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
        /// Focus the Settings Shortcuts search field (⌘F on the page).
        SettingsFind,
        /// Open the find bar over the open transcript (⌘F in Sessions).
        FindInTranscript,
        /// The transcript find's next match (⌘G).
        FindNextInTranscript,
        /// The transcript find's previous match (⇧⌘G).
        FindPrevInTranscript,
    ]
);

/// The key context the Settings page wears, so ⌘F reaches the Shortcuts
/// search field instead of the composer's (B11).
pub(crate) const SETTINGS_CONTEXT: &str = "HarnessSettings";

/// The key context the sidebar's inline rename field wears, so Enter commits
/// the name instead of reaching the composer's send.
pub(crate) const RENAME_CONTEXT: &str = "BaazRename";

/// The new-tab page's Field width (canvas NewTab board): 560.
const NEWTAB_FIELD_W: f32 = scale::SP_7 * 17.5;
/// The Browse chrome popovers' width (Downloads, Extensions, ⋯).
const BROWSE_POPOVER_W: f32 = 320.0;
/// The launch-failure screen's stderr block: its widest measure.
const LAUNCH_STDERR_MAX_W: f32 = 640.0;

/// What the host hands the shell's chrome row (see `Harness::render_bar_chrome`).
pub(crate) struct BarChrome {
    pub(crate) lead: AnyElement,
    pub(crate) switch: Option<AnyElement>,
    pub(crate) title: Option<AnyElement>,
    pub(crate) right: Option<AnyElement>,
}

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

/// The context the window wears while in Browse mode (B2a-H3): the tab
/// shortcuts (⌘W to close a tab, ⌘⇧T to reopen, ⌘1–⌘9 to select, ⌥⌘↑/↓
/// to move) bind here, so Sessions mode keeps ⌘T unbound and ⌘W closing
/// the window. Worn on a wrapper around the window body while
/// [`layout::Mode::Browse`] is current (see `render`), never in Sessions.
pub(crate) const BROWSE_CONTEXT: &str = "BaazBrowse";
/// The transcript find's keys (⌘F, ⌘G, ⇧⌘G; docs/82 7.5): the root node
/// outside Browse, whose root wears [`BROWSE_CONTEXT`] too — so Browse keeps
/// its own ⌘F — while Settings' and Skills' ⌘F bind on deeper nodes and win
/// wherever they hold focus.
pub(crate) const SESSIONS_ROOT_CONTEXT: &str = "AuiRoot && !BaazBrowse";
/// The root node's contexts in Browse: `AuiRoot` and [`BROWSE_CONTEXT`] on one
/// node (pinned against `aui::keys::ROOT_CONTEXT` by a test).
pub(crate) const BROWSE_ROOT_CONTEXT: &str = "AuiRoot BaazBrowse";

/// How long the "Session hidden" toast's Undo stays honest.
const UNDO_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);
/// How far above dead centre a hero column sits, in points.
///
/// A column centred exactly in the pane reads low: the eye puts the optical
/// centre above the geometric one, and the sidebar and composer give the
/// window weight at the bottom already. Applied as bottom padding, so the
/// column still centres — just in a slightly shorter box.
pub(crate) const HERO_LIFT: f32 = 48.0;
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
    // The Chromium key router reads this snapshot, not gpui state — refresh
    // it on every (re)install so a rebinding moves the claimed keys too.
    crate::keymap::refresh_claimed_keys(&loaded.bindings);
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
    // a one-window app, so that is the window. Close runs the close-path
    // probe signal (never a wait); Quit runs the bounded quit cleanup, like
    // the window-close and app-quit hooks respectively.
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        crate::baaz_log!("CloseWindow");
        // T1: the close path never sleeps on the main thread — signal only,
        // no probe wait, same as the red dot hook in `main.rs`.
        crate::tier::cleanup_probes_for_close();
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
        // B7fix2: quitting for real lands up to 750 ms of debounced pane
        // state first — a Cmd-Q must never lose the pane. Only after the
        // confirm above: a cancelled quit writes nothing.
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
                                    harness.update(cx, |this, cx| {
                                        this.flush_stores();
                                        this.browser.update(cx, |browser, cx| browser.flush_at_quit(cx));
                                        crate::layout::Layout::save(cx);
                                    })
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
            // One item toggles both ways (native menus are static, so it
            // cannot rename itself to "Sessions" in Browse); the ⌥⌘B
            // shortcut reads off the keymap.
            gpui::MenuItem::action("Browse", ToggleBrowseMode),
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

pub(crate) mod agent_window;
mod browser_chat;
mod registry;
pub(crate) mod browser_handoff;
mod find;
pub(crate) mod sessions;
mod list;
mod mode_context;
mod row_menu;
mod sidebar_host;
mod peek_split;
mod titles;
mod window_sync;

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
/// a batch of one archived session. An import run (docs/72) is a batch of
/// the records it added, named by its journal run id. An undone turn or a
/// reverted file (docs/82 6.4) is its session's action, by number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UndoBatch {
    Hidden(Vec<String>),
    Archived(Vec<String>),
    Imported(String),
    /// An undone turn or a reverted file (docs/82 6.4): the session and
    /// the action's number.
    Files { session_id: String, n: u64 },
}

/// What decides the search card's status line: whether the query is blank,
/// how many sessions and files came back (finding `performance-9`), and the
/// scope it was queried in — a narrowed palette names its project.
type SearchStatusKey = (bool, usize, usize, bool, String);

pub(crate) use baaz_core::lifecycle::{LaunchFailure, ProviderOpenRetry};

/// A provider open that failed on the background turn, with what the
/// child said on stderr before it went: the adapter is gone by the time the
/// failure lands, so its tail travels with the error.
pub(crate) struct OpenFailed {
    /// The open's refusal.
    pub error: provider::ProviderError,
    /// The child's last stderr lines.
    pub stderr: Vec<String>,
}

impl From<provider::ProviderError> for OpenFailed {
    fn from(error: provider::ProviderError) -> Self {
        Self { error, stderr: Vec::new() }
    }
}

impl std::fmt::Display for OpenFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

/// The lease notice: another window holds this session, so this one is
/// read-only until it lets go.
pub(crate) const LEASE_NOTICE: &str =
    "This session is open in another window. Close it there, or start a new session.";

/// The whole application.
pub struct Harness {
    pub(crate) args: Args,
    /// The window's provider registry: every adapter's factory, the slot
    /// of the child a shared-child provider's lanes ride, and how this run
    /// opens lanes (the production factories spawn the real CLI; a scripted
    /// run and tests register stand-ins). Opens never run on the UI thread
    /// — the open path runs them on the background executor, where
    /// connecting and `OpenSession` may block.
    pub(crate) providers: crate::providers::Registry,
    pub(crate) wire: Wire,
    pub(crate) auth: Auth,
    pub(crate) login: Entity<Login>,
    /// The window's provider facts (task 4.4): the statuses, the quiet
    /// sign-out banners, the cards waiting on a press and the Codex sign-in
    /// flow, owned once and read (or observed) by every surface that shows
    /// them ([`crate::provider_model`]).
    pub(crate) provider_model: Entity<crate::provider_model::ProviderStatusModel>,
    /// The first-run Connect your providers screen (Y5): a region entity
    /// that owns whether it stands and its footer menu (`connect.rs`).
    pub(crate) connect_screen: Entity<crate::connect::ConnectScreen>,
    /// The setup sheet (D5 B2): an entity that owns the open sheet, its
    /// terminals and their focus (`setup.rs`).
    pub(crate) setup: Entity<crate::setup::SetupHost>,
    /// The session list (task 4.7): the joined rows, the stores they are
    /// joined from (index, overrides, provider records, projects,
    /// branches), the list filters, the title bookkeeping and the caches
    /// derived from them, as one model entity. A change notifies the list,
    /// never the root; the sidebar pane observes it
    /// ([`crate::session_list::SessionList`]).
    pub(crate) session_list: Entity<crate::session_list::SessionList>,
    pub(crate) active: Option<Entity<SessionView>>,
    /// Opening, switching to and replacing sessions, and the drafts
    /// with no row yet: the state machine [`sessions`] drives.
    pub(crate) open_state: sessions::OpenState,
    /// The handoff runs, the destination open in flight and the
    /// confirm dialog (H2).
    pub(crate) handoff_state: sessions::HandoffState,
    /// The sidebar column as its own view (task 4.7): it owns its scroll,
    /// hover, reveal, wheel and fade, subscribes to the session list and
    /// observes the layout global, and is embedded with gpui's
    /// `.cached(size_full)`, so a clean pane reuses its retained subtree on
    /// a transcript notify. The root pushes only what it alone knows
    /// (`Harness::compose_sidebar`) and maps the pane's
    /// [`crate::sidebar_view::SidebarEvent`]s (`Harness::on_sidebar_event`).
    pub(crate) sidebar_pane: Entity<SidebarPane>,
    /// Every live view besides `active`: the parked MRU (switching away
    /// parks a view with its fold, scroll position and draft; reopening
    /// shows it at once), the browser chat the line and the card show
    /// (docs/27 §6, R3), and one event subscription per live view. Every
    /// enumeration of views goes through it (docs/82 1.6).
    pub(crate) registry: registry::SessionRegistry,
    /// Everything that floats: the overlay stack (dialog, palette, menus,
    /// popovers, sheets, renames — [`crate::overlay_stack`]) and the toasts.
    /// One entity, shared with the views that draw an entry (the session
    /// view's composer menus, the right pane's, the Browse view's).
    pub(crate) overlays: Entity<Overlays>,
    /// The right pane (task 4.3): its cache, refresh, watch, divider,
    /// focus, menus and drafts live on the entity; the root composes its
    /// view, seats its tab row, and maps its events
    /// ([`crate::right_pane::RightPaneEvent`]).
    pub(crate) right_pane: Entity<crate::right_pane::RightPane>,
    /// Whether the window's traffic lights are hidden now (closed sidebar,
    /// no peek), so the native call runs only on a change.
    pub(crate) lights_hidden: bool,
    /// Browse mode and the browser pane, one region entity (task 4.5): the
    /// tab model and one live `WebviewState` per tab (each observed: the
    /// page reconcile runs on its changes), the Field (omnibox) and find
    /// inputs, the Browse sidebar's menus and drags, the overlay windows
    /// over the native page, the agent window, `browser.db` behind its
    /// service and the one owner of what covers the native page
    /// ([`crate::browser::BrowserView`]). It asks the root through
    /// [`crate::browser::BrowserEvent`]s (`Harness::on_browser_event`).
    pub(crate) browser: Entity<crate::browser::BrowserView>,
    /// The capture hooks a `--screenshot` wait reads out of this window and
    /// the `--steps` runner sets (see [`crate::runtime::CaptureToken`]),
    /// the `sidebar-peek` hold among them. Handed to every session view this
    /// window opens and to `capture_and_quit`, so a second window would wait
    /// on its own. Empty without the `probe` feature.
    pub(crate) capture: CaptureToken,
    pub(crate) focus_root: FocusHandle,
    /// True only while the overlay window draws content the main window
    /// would otherwise draw (the Browse modal): its listeners then run on
    /// the main window ([`Harness::main_listener`]).
    pub(crate) hosting_overlay: bool,
    /// What the billing probe said, or `None` while it has not said it yet
    /// (spec §3.2). A probe that failed is
    /// [`Tier::Unavailable`], never `None`.
    pub(crate) tier: Option<Tier>,
    /// A probe is in flight; a second one is not started on top of it.
    pub(crate) tier_probing: bool,
    /// The probe owed after one that got no verdict.
    pub(crate) tier_retry: crate::tier::ProbeRetry,
    /// Its timer. Replacing or clearing this cancels it.
    pub(crate) tier_retry_timer: Option<gpui::Task<()>>,
    /// P7 import (docs/72): the cached scan plan, the open sheet, the run in
    /// flight and the journal Undo reads ([`crate::import_ui`]).
    pub(crate) import: crate::import_ui::ImportUi,
    /// The current project: the open session's project, else the last used.
    pub(crate) current_project: Option<String>,
    /// The window's lifecycle off the frame (task 4.9): its handle, the
    /// queued settle and floats passes, the composer's one-shot focus and
    /// the observations that drive them ([`window_sync::WindowSync`]).
    pub(crate) window_sync: window_sync::WindowSync,
    /// The terminal dock under the composer: its tabs (keyed by project
    /// root, D43), focus, resize drag and bridge providers
    /// ([`terminal::TerminalDock`], task 4.4).
    pub(crate) terminal_dock: Entity<terminal::TerminalDock>,
    /// The agent's seven tools over the unix socket (D46): sessions the
    /// app registers may list, open, run, read, screen, send and close
    /// these tabs. Dropped (and its socket removed) on quit.
    pub(crate) terminal_service: TerminalService,
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
    /// The field an inline rename types in: a session row's or the
    /// project's (which one stands is the overlay stack's `RenameSession` /
    /// `RenameProject` entry).
    pub(crate) rename: Entity<TextareaState>,
    /// The Settings page (`Route::Settings`, B11): a region entity that
    /// owns its route, search, focus, Shortcuts rows and recording, and its
    /// pages' menus and folds (`settings.rs`, task 4.4).
    pub(crate) settings_page: Entity<crate::settings::SettingsPage>,
    /// The Skills page (docs/15-skills.md §4): a region entity that owns its
    /// open flag, catalog, filter, search, selection, dialogs and CLI calls
    /// (`skills_page.rs`, task 4.4).
    pub(crate) skills_page: Entity<crate::skills_page::SkillsPage>,
    /// The search card's status line and the three facts it is made of:
    /// whether the query is blank and the two result counts (finding
    /// `performance-9`). Only ever read while the search palette is open.
    search_status: RefCell<Option<(SearchStatusKey, SharedString)>>,
    /// "Send anyway" was pressed. Once per app run, deliberately: a person who
    /// accepted the bill this morning should be asked again tomorrow.
    pub(crate) send_anyway: bool,
    pub(crate) tasks: Vec<Task<()>>,
    /// The boot-time subscriptions: the right pane's events and the text
    /// fields (rename, search, Projects and Commands queries, API key). Made
    /// once in `new`, never cleared.
    field_subscriptions: Vec<Subscription>,
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
        // The window's thread: from here on, SQLite on it outside a worker
        // job fails the `check_off_ui` assert (tests, debug builds).
        crate::db::mark_ui_thread();
        // Task 3.3: a click on a notification opens its session, and the
        // first attention look (the Dock badge) runs once construction is
        // over — whichever of the boot paths below returns, scripted
        // fixture and replay windows included, which raise no event.
        Self::on_notification_click(cx);
        let first_look = cx.entity().downgrade();
        cx.defer(move |cx| {
            if let Some(harness) = first_look.upgrade() {
                harness.update(cx, |harness, cx| harness.check_attention(cx));
            }
        });
        // The rename field is one visual line: soft wrap off, so a long name
        // scrolls under the caret instead of spilling a second line.
        let rename = cx.new(|cx| {
            let mut state = composer_state_rows("Name this session", 1, 1, window, cx);
            state.set_soft_wrap(false, window, cx);
            state
        });
        let search_query = cx.new(|cx| composer_state_rows("Search sessions and created files", 1, 1, window, cx));
        // The session list, and the sidebar column over it: the pane holds
        // the list and the rename field, never this entity.
        // The divider's last settled x, or the default for a fresh store.
        // Window preferences live in one gpui global (`layout.json`), read
        // once here when `main` has not already: a test window boots it.
        layout::Layout::init(cx);
        let restored = layout::sidebar_width(layout::Layout::get(cx));
        let session_list = cx.new(|_| crate::session_list::SessionList::new());
        // The pane owns the divider's drag, starting at the restored width.
        let sidebar_pane = {
            let (list, rename) = (session_list.clone(), rename.clone());
            cx.new(move |cx| SidebarPane::new(list, rename, restored, cx))
        };
        // The API-key field: masked, with the capture-safe placeholder. Enter
        // inside it submits (single-line inputs always emit `PressEnter`).
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste your key"));
        // The right divider's last settled width, or the default: the pane
        // starts there, sharing the window's floating layer for its toasts.
        let overlays = cx.new(|_| Overlays::default());
        let right_restored = layout::right_width(layout::Layout::get(cx));
        // The terminal's tabs: the dock draws them, the service serves the
        // agent's tools over them, and the pane's Tasks screen lists what the
        // agent started there (docs/82 6.5).
        let terminal_host = cx.new(|_| TerminalHost::new());
        // The dock draws the same tabs the service serves, and shares the
        // window's floating layer for its toasts. The pane is given it too:
        // its Terminal view is the dock's own body (docs/82 8.1).
        let terminal_dock = cx.new(|cx| terminal::TerminalDock::new(terminal_host.clone(), overlays.clone(), cx));
        let right_pane = cx.new(|cx| crate::right_pane::RightPane::new(right_restored, overlays.clone(), terminal_host.clone(), terminal_dock.clone(), cx));
        // The Skills page, closed: it lists for the launch workspace until
        // the root pushes the current project.
        // It also holds where new sessions start: `--provider`, else the
        // stored pick while it can start one, else the first that can run
        // in the one provider order — following the statuses until the person picks.
        let explicit = args.provider_explicit.then(|| args.provider.clone());
        let provider_model = cx.new(|_| crate::provider_model::ProviderStatusModel::new(overlays.clone(), explicit));
        let connect_screen = cx.new(|cx| crate::connect::ConnectScreen::new(provider_model.clone(), overlays.clone(), cx));
        let setup = cx.new(|cx| crate::setup::SetupHost::new(provider_model.clone(), overlays.clone(), cx));
        let settings_page = cx.new(|cx| crate::settings::SettingsPage::new(provider_model.clone(), overlays.clone(), window, cx));
        let skills_page = {
            let program = args.program.clone();
            let root = args.workspace.to_string_lossy().into_owned();
            let overlays = overlays.clone();
            cx.new(|cx| crate::skills_page::SkillsPage::new(program, root, overlays, window, cx))
        };
        // The backend new sessions start on: an explicit `--provider` or
        // `BAAZ_PROVIDER` names this run; otherwise the last pick the
        // person made, kept in the store across relaunches; otherwise the
        // command-line default.
        // The terminal service binds before the first frame: its socket is
        // how the agent's routes reach the tabs, and a relay started
        // beside the app must find it already listening.
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
        // Z7a, read once at boot: captures (`--screenshot`) and tests run the
        // scripted browser page — a native view never appears in a gpui
        // screenshot, and captures must be deterministic — while the app runs
        // WKWebView. Never sniffed in render.
        let browser_fake = args.screenshot.is_some();
        let browser = cx.new(|cx| crate::browser::BrowserView::new(browser_fake, overlays.clone(), cx));
        // The runtime's provider source, made concrete for this window.
        let providers = crate::providers::Registry::for_window(
            crate::runtime::lanes_for(crate::runtime::Lanes::scripted_if(args.offline || args.replay.is_some())),
            terminal_service.socket_path().to_path_buf(),
        );
        let focus_root = cx.focus_handle();
        let mut this = Self {
            args,
            providers,
            wire: Wire::Connecting,
            auth: Auth::Probing,
            login: cx.new(|_| Login::new(api_key.clone())),
            provider_model,
            connect_screen,
            setup,
            session_list,
            active: None,
            open_state: sessions::OpenState::default(),
            handoff_state: sessions::HandoffState::default(),
            sidebar_pane,
            registry: registry::SessionRegistry::default(),
            overlays,
            right_pane,
            browser,
            lights_hidden: false,
            capture,
            focus_root,
            hosting_overlay: false,
            tier: None,
            tier_probing: false,
            tier_retry: Default::default(),
            tier_retry_timer: None,
            import: crate::import_ui::ImportUi::boot(),
            current_project: None,
            terminal_dock,
            terminal_service,
            search_query: search_query.clone(),
            search_sessions: Vec::new(),
            search_files: Vec::new(),
            search_epoch: 0,
            rename: rename.clone(),
            settings_page,
            skills_page,
            search_status: RefCell::new(None),
            send_anyway: false,
            tasks: Vec::new(),
            field_subscriptions: Vec::new(),
            window_sync: window_sync::WindowSync::new(),
        };
        // Every window preference lives in the layout global (task 4.1): a
        // change from anywhere — a Settings switch, a menu, a step, the
        // terminal hook — redraws the window, with no notify at the write.
        cx.observe_global::<layout::LayoutStore>(|_, cx| cx.notify()).detach();
        // Settings › Appearance › System follows macOS: an appearance
        // change repaints while the stored choice is System (a Light or
        // Dark choice ignores it). Detached: it lives as long as the window.
        cx.observe_window_appearance(window, |this, window, cx| {
            if crate::layout::Layout::get(cx).theme == layout::ThemePref::System {
                this.apply_theme_pref(Some(window), cx);
            }
        })
        .detach();
        // Booting into Browse stands its right pane closed, like a switch.
        let browse = layout::Layout::get(cx).mode == layout::Mode::Browse;
        if browse {
            layout::Layout::stage(cx, layout::Layout::enter_browse);
        }
        // The CEF claim check's live-mode flag starts where the window
        // starts; every switch after this rides `set_mode`.
        crate::keymap::set_claim_live_browse(browse);
        // When the agent opens a URL the Browser pane opens on it — never
        // focused, so the person's keyboard stays where it was while they
        // watch. Deferred for the same re-entrancy reason as the dock hook
        // below: the hook runs inside `drain`, inside a Harness update.
        let browser_harness = cx.entity();
        this.terminal_service.set_browser_open_hook(move |cx: &mut App, session: String| {
            let harness = browser_harness.clone();
            cx.defer(move |cx| {
                harness.update(cx, |harness, cx| {
                    // A browser chat browses in tabs of its own, in Browse
                    // (its task folder), never in a Sessions pane (R3).
                    if harness.is_browser_chat(&session, cx) {
                        harness.run_on_main(cx, move |harness, window, cx| harness.open_browser_chat_tab(&session, window, cx));
                    } else {
                        harness.show_browser_for_agent(&session, cx);
                    }
                });
            });
        });
        let tabs_harness = cx.entity();
        this.terminal_service.set_browser_tabs_hook(move |cx: &mut App, call| {
            let harness = tabs_harness.clone();
            cx.defer(move |cx| {
                harness.update(cx, |harness, cx| harness.agent_tabs_call(call, cx));
            });
        });
        // When the agent runs something the terminal shows where it sits —
        // never focused, so the person's keyboard stays where it was while
        // they watch (`reveal_terminal_for_agent`).
        let activity_harness = cx.entity().downgrade();
        this.terminal_service.set_activity_hook(move |cx: &mut App| {
            // Deferred to the end of the effect cycle, so it lands after the
            // `drain` (run inside a Harness update) that reported the
            // activity.
            let harness = activity_harness.clone();
            cx.defer(move |cx| {
                let _ = harness.update(cx, |harness, cx| harness.reveal_terminal_for_agent(cx));
            });
        });
        // The service's requests queue on socket threads; this pump runs
        // them on the UI thread until the window is gone. A pass is one
        // `drain`: queued tools answer and waiting runs are polled, so a
        // long run never stalls a frame. Passes come every 15 ms only while
        // something waits; otherwise the pump sleeps until a socket thread
        // queues a job (a 2 s fallback pass covers anything that slipped
        // past the signal). It used to wake the UI thread 66 times a second
        // forever (perf audit 2026-10-16).
        let mut woken = this.terminal_service.take_wake();
        this.tasks.push(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            loop {
                let busy = match this.update(cx, |this, _| this.terminal_service.busy()) {
                    Ok(busy) => busy,
                    Err(_) => return,
                };
                match woken.as_mut() {
                    Some(rx) if !busy => {
                        let timer = cx.background_executor().timer(std::time::Duration::from_secs(2));
                        futures::pin_mut!(timer);
                        let closed = matches!(futures::future::select(rx.next(), timer).await, futures::future::Either::Left((None, _)));
                        while rx.try_recv().is_ok() {}
                        if closed {
                            // A closed signal would answer at once forever:
                            // fall back to the timer.
                            woken = None;
                        }
                    }
                    _ => cx.background_executor().timer(std::time::Duration::from_millis(15)).await,
                }
                let alive = this.update(cx, |this, cx| {
                    let before = this.terminal_service.activity_print();
                    this.terminal_service.drain(cx);
                    // The line and the task folders follow the agent's page work:
                    // a window at rest draws nothing until told.
                    if this.terminal_service.activity_print() != before {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
        // A provider probe landed off the UI thread: re-read the statuses
        // now. Nothing renders at rest (P0), so waiting for a frame left a
        // finished probe — an install, a sign-in — unseen until the pointer
        // moved (docs/65 §1).
        let mut status_wake = crate::provider_status::window_status_wake();
        this.tasks.push(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while status_wake.next().await.is_some() {
                while status_wake.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.sync_provider_state(cx)).is_err() {
                    return;
                }
            }
        }));
        // The machine woke: a tier probe that got no verdict gets another go.
        let mut wake = crate::watchdog::wake_channel();
        this.tasks.push(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while wake.next().await.is_some() {
                if this.update(cx, |this, cx| this.tier_probe_on_wake(cx)).is_err() {
                    return;
                }
            }
        }));
        // Typing in the rename field redraws the row being renamed — in the
        // sidebar pane, which owns that element, as well as here for the
        // header title that shares the field.
        // The right pane's asks (persist, open on a kind, a URL, a draft…).
        let pane = this.right_pane.clone();
        this.field_subscriptions.push(cx.subscribe_in(&pane, window, Self::on_right_pane_event));
        // The dock's asks (a link, a busy tab's close, a block for the draft).
        let dock = this.terminal_dock.clone();
        this.field_subscriptions.push(cx.subscribe_in(&dock, window, Self::on_terminal_dock_event));
        // The Skills page's asks (the project menu, a Muse session for A5).
        let skills = this.skills_page.clone();
        this.field_subscriptions.push(cx.subscribe_in(&skills, window, Self::on_skills_page_event));
        let settings = this.settings_page.clone();
        this.field_subscriptions.push(cx.subscribe_in(&settings, window, Self::on_settings_event));
        let connect = this.connect_screen.clone();
        this.field_subscriptions.push(cx.subscribe_in(&connect, window, Self::on_connect_event));
        // The setup sheet's host draws every setup sheet (it keeps the
        // terminals across them): its dismissal pops the overlay stack.
        let setup = this.setup.clone();
        this.field_subscriptions.push(cx.subscribe(&setup, |this: &mut Self, setup, _: &gpui::DismissEvent, cx| {
            this.dismiss_overlay_view(setup.entity_id(), cx);
        }));
        let model = this.provider_model.clone();
        this.field_subscriptions.push(cx.subscribe(&model, Self::on_provider_model_event));
        // The Browse view's asks (task 4.5b): its pages changed (the root's
        // half of the reconcile), the discard sweep, the done task folders.
        // `browser.db` landings redraw the view itself, never the root.
        let browser = this.browser.clone();
        this.field_subscriptions.push(cx.subscribe_in(&browser, window, Self::on_browser_event));
        // The Bookmarks and History page (docs/82 8.6): made here, where the
        // window is (its search field needs one), owned by the Browse view.
        let data = browser.read(cx).data.clone();
        let library = cx.new(|cx| crate::browser::library::LibraryPage::new(data, window, cx));
        this.field_subscriptions.push(cx.subscribe_in(&library, window, |this, _, event: &crate::browser::library::LibraryEvent, window, cx| {
            this.on_library_event(event.clone(), window, cx);
        }));
        browser.update(cx, |browser, _| browser.library = Some(library));
        // The sidebar column's intents and wake-ups (task 4.7).
        let sidebar = this.sidebar_pane.clone();
        this.field_subscriptions.push(cx.subscribe_in(&sidebar, window, Self::on_sidebar_event));
        // The default-browser cache (B2a-H7b): render reads the cache only,
        // so the LaunchServices lookup runs at launch and on every regained
        // focus (`Self::on_window_activation`).
        this.browser.update(cx, |browser, cx| browser.refresh_default_browser_cache(cx));
        this.field_subscriptions.push(cx.subscribe(&rename, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sidebar_pane.update(cx, |_, cx| cx.notify());
            }
        }));
        // Typing in the search palette's query re-queries off the UI thread.
        this.field_subscriptions.push(cx.subscribe(&search_query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.refresh_search(cx);
            }
        }));
        // The API-key field: Enter submits. A keystroke redraws the window
        // the field is drawn in, and the Muse sheet works `can_submit` out
        // from the field's text as it draws.
        this.field_subscriptions.push(cx.subscribe(&api_key, |this: &mut Self, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_api_key(cx);
            }
        }));
        this.session_list.update(cx, |list, _| list.overrides = crate::sessions::read());
        // A damaged store was set aside while booting, or a background write
        // fails later: one toast each (docs/81 A F8, F9). The wake channel
        // is closed in tests, like the status wake: those call
        // `raise_store_problems` themselves.
        if !cfg!(test) {
            this.raise_store_problems(cx);
        }
        let mut store_wake = if cfg!(test) {
            futures::channel::mpsc::unbounded().1
        } else {
            crate::store::problem_wake()
        };
        this.tasks.push(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while store_wake.next().await.is_some() {
                while store_wake.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.raise_store_problems(cx)).is_err() {
                    return;
                }
            }
        }));
        // A restart mid-flight still hides every side session: the persisted
        // `side_session` flags rejoin the in-memory record at boot.
        this.session_list.update(cx, |list, _| {
            list.side_sessions =
                list.overrides.iter().filter(|(_, meta)| meta.side_session).map(|(id, _)| id.clone()).collect();
        });
        // Before anything reads it: "New session" with no project starts
        // here, and on a first launch that click comes before the person has
        // adopted anything at all.
        projects::ensure_default_workspace();
        this.boot_projects(cx);
        // `--tier` is a scripted answer to a probe that has not been run, and
        // it applies to every mode — including `--replay`, which is how the
        // banner is captured for nothing.
        this.tier = this.args.tier.clone();
        // The window's lifecycle runs from its causes, never from a frame
        // (task 4.9): the settle, the floats, the title, focus, activation.
        this.install_window_sync(window, cx);
        // The two capture boots (`probe` builds only: without it neither
        // flag parses).
        #[cfg(feature = "probe")]
        if this.args.replay.is_some() {
            // `--replay`: a capture, folded, with no child and no credential.
            // The shell is the point — the transcript is what is being looked
            // at — so the auth probe is skipped rather than faked into a login.
            this.auth = Auth::SignedIn(Identity {
                lane: provider::AccountLane::AccountLogin,
                name: "Replay".into(),
                email: String::new(),
            });
            this.wire = Wire::Ready;
            // The capture opens here, while the root is built, so the first
            // frame draws the folded rows and measures them (it used to open
            // from that frame's render). Deferred, it landed after
            // `open_window` had drawn frame one: an offscreen capture window
            // draws no other frame before the shot, so the rows stayed at
            // their estimated heights and a `wheel:` step at a fixed delay
            // scrolled estimates — the jump-pill captures lost their pill.
            this.open_replay(window, cx);
            return this;
        }
        #[cfg(feature = "probe")]
        if this.args.offline {
            // `--no-connect`: the chrome without a child, for a screenshot of
            // the login screen with sample data (`--login` picks the state).
            this.auth = Auth::SignedOut;
            this.wire = Wire::Down("not connected".into());
            this.apply_login_sample(window, cx);
            // A scripted sidebar draws without a list reply — but only when
            // one was asked for, so the login captures draw as they did.
            if this.args.sidebar_fixture.is_some() {
                this.apply_sidebar_fixture(cx);
                this.session_list.update(cx, |list, _| list.sessions_loaded = true);
                this.session_list.update(cx, |list, _| list.index_loaded = true);
                this.invalidate_list(cx);
            }
            return this;
        }
        // The launch decision from stored facts only: a first run shows the
        // Connect your providers screen; every other launch renders the
        // shell at once from the cached statuses. Muse's `account/read`
        // still reports (and refreshes Muse's row), but never gates the
        // window.
        let first_run = crate::connect::decide_launch(crate::provider_status::first_run_at_boot())
            == crate::connect::LaunchDecision::Connect;
        if first_run {
            this.connect_screen.update(cx, |screen, _| screen.show_first_run());
        }
        // P7: the first-run connect screen offers the import once a
        // provider is Connected; its counts come from a background scan.
        this.import.first_run = first_run;
        if first_run {
            this.refresh_import_scan(cx);
        }
        // Muse is one provider among three (docs/50 P1): it starts only when
        // the person has it on and it was not found missing. Otherwise the
        // shell settles on the local and provider records at once.
        if crate::provider_status::muse_wanted() {
            this.connect(cx);
        } else {
            this.muse_unavailable(crate::connect::shared_off(), cx);
        }
        this.load_index(cx);
        // Every live worktree's branch and variables, read once off the UI
        // thread, so the rows and the header name a worktree session's
        // branch (docs/82 6.2) without a database read on a frame.
        if !crate::runtime::state_isolated() {
            let state_dir = crate::store::support_dir();
            this.wire_call_in(cx, move || baaz_core::worktrees::remember_all(&state_dir), |this, known, _, cx| {
                if known > 0 {
                    this.invalidate_list(cx);
                }
            });
        }
        // Only when a project is current. With none — a first launch, or a
        // bundle opened from Finder, where `boot_projects` declined to adopt
        // `/` — `workspace()` falls back to the launch directory, which is not
        // a workspace and holds nothing worth priming a picker with. Adopting
        // a project, or starting a session in a folder, loads them then.
        if this.current_project.is_some() {
            this.load_menu_sources(std::path::PathBuf::from(this.workspace(cx)), cx);
        }
        // B4M's one-time prompt: deferred past construction (the hook runs
        // inside `drain`, where touching the Harness re-enters it — the
        // defer runs at the end of the effect cycle instead). The offer
        // itself declines over a connect screen, a capture, a dismissal,
        // an existing dialog, or an empty plan — building nothing.
        let migration_harness = cx.entity();
        cx.defer(move |cx| {
            migration_harness.update(cx, |harness, cx| {
                harness.maybe_offer_session_migration(cx);
            });
        });
        crate::log::boot_mark("harness-new-done");
        this
    }

    /// The workspace path as the wire and the header want it: the current
    /// project's root, else the launch workspace.
    pub(crate) fn workspace(&self, cx: &gpui::App) -> String {
        self.current_project(cx)
            .map(|p| p.root.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.args.workspace.to_string_lossy().into_owned())
    }

    /// Where a session that belongs to no project starts, on every
    /// provider: the folder `--workspace` named, else the default
    /// workspace (`~/baaz-sessions`) — never the launch directory, which is
    /// `/` from Finder (docs/81 F C1).
    pub(crate) fn unfiled_workspace(&self) -> String {
        if self.args.workspace_explicit {
            return self.args.workspace.to_string_lossy().into_owned();
        }
        projects::default_workspace().to_string_lossy().into_owned()
    }

    /// The current project's id, when one is current.
    pub(crate) fn current_project_id(&self) -> Option<String> {
        self.current_project.clone()
    }

    /// The current project: the open session's project, else the last used —
    /// else the most recently opened adoption whose root is on disk. A
    /// missing root is never current, but stays adopted.
    pub(crate) fn current_project<'a>(&self, cx: &'a gpui::App) -> Option<&'a Project> {
        self.current_project
            .as_deref()
            .and_then(|id| self.session_list.read(cx).projects.find_available(id))
            .or_else(|| self.session_list.read(cx).projects.most_recent_available())
    }

    /// The header and window title for a chain head: the collapsed view
    /// row's label — the same one the sidebar's one row wears — so header
    /// and row can never disagree (Y2a3). Falls back to the stored row's
    /// title ladder when the head is filtered out of the view.
    pub(crate) fn collapsed_head_label(&self, head: &str, cx: &gpui::App) -> Option<String> {
        // One row-line, cut where the row would truncate it anyway: a pasted
        // brief as the derived title can never fill the whole header, and
        // the row and the header read the same words.
        if let Some(entry) = self.visible_sessions(cx).iter().find(|e| e.id == head) {
            let pending = entry.title_pending || self.session_list.read(cx).titles_pending.contains(&entry.id);
            return Some(crate::sidebar::one_line(crate::sidebar::display_label(
                &entry.label,
                pending,
            )));
        }
        self.session_list.read(cx).sessions.iter().find(|e| e.id == head).map(|e| {
            let pending = e.title_pending || self.session_list.read(cx).titles_pending.contains(&e.id);
            let text = if !e.named {
                sidebar::handoff_title_of(head, &self.session_list.read(cx).overrides)
                    .map(|t| sidebar::one_line(&t))
                    .unwrap_or_else(|| e.label.clone())
            } else {
                e.label.clone()
            };
            crate::sidebar::one_line(crate::sidebar::display_label(&text, pending))
        })
    }

    /// What the window's own title bar says: the open session against its
    /// project, the project alone, or the app name with no project at all.
    fn window_title(&self, cx: &gpui::App) -> String {
        let session = self.active.as_ref().map(|a| a.read(cx).rt(cx).session_id.clone());
        // One identity per chain: the raw view id resolves to its head,
        // and the collapsed view row — the same label the sidebar's one
        // row wears — names the window (Y2a, Y2a3).
        let label = session.and_then(|id| self.collapsed_head_label(&self.chain_head(&id, cx), cx));
        match self.current_project(cx) {
            Some(project) => match label {
                Some(label) => format!("{label} \u{2014} {}", project.name),
                None => project.name.clone(),
            },
            None => "Baaz".to_owned(),
        }
    }

    /// The current project's name, which is what the header and the empty
    /// state show — or the app name when no project is current.
    fn workspace_name(&self, cx: &gpui::App) -> String {
        self.current_project(cx).map(|p| p.name.clone()).unwrap_or_else(|| "Baaz".to_owned())
    }

    /// Read the projects store and settle the current project (decision D39):
    /// an explicit `--workspace` wins and is adopted if new; a scripted run's
    /// launch directory keeps today's semantics by the same rule; else the
    /// stored current project if it still exists; else the most recently
    /// opened adoption; else the launch directory, unless it is `/` or
    /// `$HOME`, where the window opens with no project and the hero owns the
    /// empty state. The file is written when anything above changed it.
    pub(super) fn boot_projects(&mut self, cx: &mut gpui::App) {
        // `--no-project` boots the hero: nothing adopted, nothing current.
        if self.args.no_project {
            self.session_list.update(cx, |list, _| list.projects = Projects::default());
            self.current_project = None;
            return;
        }
        self.session_list.update(cx, |list, _| list.projects = projects::read());
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
            let id = self.session_list.update(cx, |list, _| list.projects.add(&workspace).id.clone());
            self.session_list.update(cx, |list, _| list.projects.touch(&id));
            if self.session_list.read(cx).projects.current.as_deref() != Some(id.as_str()) {
                self.session_list.update(cx, |list, _| list.projects.current = Some(id));
            }
            dirty = true;
        } else if self.session_list.read(cx).projects.current.as_deref().is_some_and(|id| self.session_list.read(cx).projects.find_available(id).is_some()) {
            // The stored current project still exists on disk: keep it, untouched.
            // A missing root is not current (it stays adopted, and comes back
            // when the path does).
        } else if let Some(recent) = self.session_list.read(cx).projects.most_recent_available().map(|p| p.id.clone()) {
            self.session_list.update(cx, |list, _| list.projects.current = Some(recent));
            dirty = true;
        } else {
            let launch = self.args.workspace.clone();
            if projects::is_workspace_root(&launch) {
                let id = self.session_list.update(cx, |list, _| list.projects.add(&launch).id.clone());
                self.session_list.update(cx, |list, _| list.projects.touch(&id));
                self.session_list.update(cx, |list, _| list.projects.current = Some(id));
                dirty = true;
            } else if self.session_list.read(cx).projects.current.is_some() {
                self.session_list.update(cx, |list, _| list.projects.current = None);
                dirty = true;
            }
        }
        self.current_project = self.session_list.read(cx).projects.current.clone();
        if dirty {
            projects::write(&self.session_list.read(cx).projects);
        }
    }

    // ------------------------------------------------------------ connection

    /// Start the shared child (spawn, initialize), and start draining its
    /// events.
    pub(crate) fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(shared_id) = crate::providers::Registry::shared_id() else { return };
        crate::log::boot_mark("connect-sent");
        // The path set in Settings (D5 B4) unless an env override names one.
        let env_named = shared_id
            .descriptor()
            .env_overrides
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()));
        let program = match crate::details::path_override(shared_id) {
            Some(chosen) if !env_named => chosen.to_string_lossy().into_owned(),
            _ => self.args.program.clone(),
        };
        // Its sessions' terminal relay rides the client-tools grant.
        let relay = crate::providers::terminal_relay(self.terminal_service.socket_path());
        let providers = self.providers.clone();
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                let shared = providers.shared().ok_or_else(|| provider::ProviderError::Unavailable {
                    reason: "no provider shares a child".into(),
                })?;
                let out = conn::connect(shared, &program, relay);
                crate::log::boot_mark(&format!("connect-work-done ok={} in={}ms", out.is_ok(), at.elapsed().as_millis()));
                out
            },
            |this, result, cx| match result {
            Ok(connected) => {
                crate::log::boot_mark("connect-reply");
                crate::baaz_log!("connected to {} {}", connected.agent_name, connected.agent_version);
                if let Some(warning) = &connected.warning {
                    // A fingerprint mismatch is additive evolution, never a
                    // failure: say so on stderr and carry on.
                    crate::baaz_log!("{warning}");
                }
                this.wire = Wire::Ready;
                this.observe_provider(connected.events, cx);
                this.probe_account(cx);
                cx.notify();
            }
            Err(error) => {
                // A Muse the probe found installed failing to start is a
                // fault worth a dialog; an absent or unprobed one is just
                // a provider this person does not use.
                if crate::provider_status::muse_known_installed() {
                    this.set_dialog(cx, Dialog {
                        title: format!("{} could not be started", crate::providers::shared_label()),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Try again",
                        action: DialogAction::Reconnect,
                        archive_target: None,
                    });
                } else {
                    crate::baaz_log!("muse not started: {error}");
                }
                this.muse_unavailable(error.to_string(), cx);
            }
        });
    }

    /// Whether the shell is on screen: not behind the connect screen. What
    /// links from other apps wait for. (The capture-only whole-window login
    /// screen is gone, docs/82 0.16: no launch ever showed it.)
    pub(crate) fn shell_up(&self, cx: &App) -> bool {
        !self.connect_open(cx)
    }

    /// Muse is not running (off, missing, or failed to start): the sidebar
    /// settles on what the local and provider records hold instead of
    /// waiting for a `session/list` that will never come, and the other
    /// providers' rows show at once.
    pub(crate) fn muse_unavailable(&mut self, reason: String, cx: &mut Context<Self>) {
        if let Some(shared) = self.providers.shared() {
            shared.set_down(reason.clone());
        }
        self.wire = Wire::Down(reason);
        // A Muse draft waiting on the start stops saying "Starting Muse…".
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_muse_starting(false, cx)));
        }
        if !self.session_list.read(cx).sessions_loaded {
            self.session_list.update(cx, |list, _| list.sessions_loaded = true);
            self.merge_provider_rows(cx);
            self.invalidate_list(cx);
        }
        cx.notify();
    }

    /// Observe the shared child's connection stream: the window's own facts
    /// (row live state, sign-in, usage) and the child's loss, which starts
    /// the reconnect. Session views hear their own lanes. It ends when the
    /// stream's sender is dropped.
    fn observe_provider(&mut self, mut events: UnboundedReceiver<ProviderEvent>, cx: &mut Context<Self>) {
        self.tasks.push(cx.spawn(async move |this, cx| {
            // The hub keeps one connection stream across respawns, so this
            // outlives a child's exit and hears the next child's events.
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |this, cx| {
                        this.on_hub_event(event, cx);
                        this.check_attention(cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    /// One event off the hub's connection stream: the rows' live facts for
    /// every Muse session (open or not), sign-in, usage, and the child's
    /// exit. Session views hear their own lanes; this is the window's.
    fn on_hub_event(&mut self, event: ProviderEvent, cx: &mut Context<Self>) {
        match event {
            ProviderEvent::ConnectionLost { .. } => {
                // The exit reason: whatever the child said on stderr last.
                let tail = self.providers.shared().map(|shared| shared.stderr_tail()).unwrap_or_default();
                if tail.is_empty() {
                    crate::baaz_log!("muse serve exited; reconnecting");
                } else {
                    crate::baaz_log!("muse serve exited; reconnecting; stderr tail: {tail}");
                }
                self.wire = Wire::Reconnecting;
                self.reconnect(cx);
            }
            // Sign-in rides the account events; the session views never
            // see them.
            event @ (ProviderEvent::AccountChanged { .. } | ProviderEvent::LoginCompleted { .. }) => {
                self.on_account_event(event, cx);
            }
            // A new reading keeps the tier live past its boot-time one: the
            // footer meter and the banner follow through `push_tier`.
            ProviderEvent::UsageChanged { report } => self.apply_usage_changed(&report, cx),
            // A Muse turn on the shared connection. Rows, records and titles
            // follow the view's own lane (`ProviderTurnAccepted` /
            // `ProviderTurnFinished`), like every provider (docs/70); this
            // stream only moves the one row's live state — for a session
            // with no view here (the muse CLI, another window), and for a
            // viewed one whose turn started elsewhere, which raises no
            // admission in this window. `sync_row_live` declines a start
            // the open view has already seen finish. Never a relist.
            ProviderEvent::TurnRunning { session_id, .. } => {
                self.sync_row_live(&session_id, true, cx);
            }
            ProviderEvent::TurnSettled { session_id, outcome, .. } => {
                // The turn's terminal, recorded into the row and the store: a
                // failed turn's message is what `Failed` stands on, and a
                // later success (or a retry's start) stands it down. Only
                // this stream carries the outcome, so it records for every
                // Muse session, viewed or not.
                let (failed, error) = match outcome {
                    provider::facts::TurnOutcome::Failed { message, .. } => (true, message),
                    _ => (false, None),
                };
                self.sync_row_live(&session_id, false, cx);
                self.record_turn_outcome(&session_id, failed, error.as_deref(), cx);
                // A finished turn is when the index has something new to say
                // about the session; the read is fingerprinted and cheap.
                self.load_index(cx);
            }
            // A loaded session's `(status, attention)` flipped: folded into
            // the row for sessions whose view is not open; the open session
            // then overlays its own truth, which is fresher than any
            // listing.
            ProviderEvent::StatusChanged { session_id, running, attention } => {
                if !self.is_side_session(&session_id, cx) {
                    let found = self.session_list.update(cx, |list, _| {
                        list.sessions
                            .iter_mut()
                            .find(|entry| entry.id == session_id)
                            .map(|entry| entry.apply_status_changed(running, &attention))
                            .is_some()
                    });
                    if found {
                        self.invalidate_list(cx);
                    }
                }
                self.sync_row_live(&session_id, false, cx);
            }
            _ => return,
        }
        if let Some(view) = self.active.clone() {
            let runtime = view.read(cx).runtime().clone();
            self.title_from_transcript(&runtime, cx);
        }
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
        // Never connected: this is the first start, not a respawn.
        if !self.providers.shared().is_some_and(|shared| shared.started()) {
            self.wire = Wire::Connecting;
            self.connect(cx);
            return;
        }
        if let Some(shared) = self.providers.shared() {
            shared.set_starting();
        }
        let providers = self.providers.clone();
        let work = move || match providers.shared() {
            Some(shared) => shared.reconnect().map(|refused| (providers.clone(), refused)),
            None => Err(provider::ProviderError::Unavailable { reason: "no provider shares a child".into() }),
        };
        self.wire_call(cx, work, move |this, result, cx| match result {
            Ok((providers, refused)) => {
                let Some(shared) = providers.shared() else { return };
                if let Some(warning) = shared.schema_warning() {
                    // A fingerprint mismatch is additive evolution, never a
                    // failure: say so on stderr and carry on.
                    crate::baaz_log!("{warning}");
                }
                shared.set_ready();
                this.wire = Wire::Ready;
                // Every followed session the respawned child re-attached has
                // its lease back; one it would not re-attach keeps its view
                // and history: another window holding it reads as the lease
                // notice, anything else says what went wrong.
                let refused_ids: Vec<String> = refused.iter().map(|(id, _)| id.clone()).collect();
                for session_id in shared.followed() {
                    if refused_ids.contains(&session_id) {
                        continue;
                    }
                    if let Some(view) = this.find_view(&session_id, cx) {
                        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.note_resumed(cx)));
                    }
                }
                for (session_id, error) in refused {
                    let view = this.find_view(&session_id, cx);
                    match error.hint() {
                        provider::RefusalHint::HeldElsewhere => {
                            crate::baaz_log!("reconnect resume of {session_id} rejected ({error}); lease notice on the view");
                            if let Some(view) = view {
                                view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_lease_lost(LEASE_NOTICE, cx)));
                            }
                        }
                        provider::RefusalHint::StaleHistory => {
                            crate::baaz_log!("reconnect resume of {session_id} hit a stale history ({error}); history stays");
                        }
                        _ => {
                            crate::baaz_log!("reconnect resume of {session_id} failed: {error}");
                            if let Some(view) = view {
                                let text = error.to_string();
                                view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.show_banner(&text, cx)));
                            }
                        }
                    }
                }
                cx.notify();
            }
            Err(error) => {
                if let Some(shared) = this.providers.shared() {
                    shared.set_down(error.to_string());
                }
                this.wire = Wire::Down(error.to_string());
                this.set_dialog(cx, Dialog {
                    title: conn::provider_title(&error),
                    detail: error.to_string(),
                    kind: DialogKind::Error,
                    primary: "Reconnect",
                    action: DialogAction::Reconnect,
                    archive_target: None,
                });
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
        self.args.screenshot.is_some() || crate::runtime::captures()
    }

    /// The sidebar's collapse toggle (⌘B): the rail is the column at zero
    /// width. One state for both modes.
    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        layout::Layout::update(cx, |layout| layout.sidebar_open = !layout.sidebar_open);
        // A Browse menu hangs off a sidebar row or the page controls: with
        // the sidebar gone it pointed at nothing (seen live 2026-10-12, the
        // page menu stayed where the sidebar had been).
        self.close_browse_menu(cx);
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
        cx: &mut Context<Self>,
    ) {
        // A worktree session's terminal starts with the worktree's
        // variables: learn them off the UI thread now, for the first tab.
        if crate::app::Harness::in_worktree(&project_root.to_string_lossy()) {
            let (state_dir, folder) = (crate::store::support_dir(), project_root.clone());
            cx.background_spawn(async move {
                baaz_core::worktrees::env_for_folder(&state_dir, &folder);
            })
            .detach();
        }
        self.terminal_service.register_session(session_id, project_root);
        self.terminal_dock.update(cx, |dock, _| dock.note_provider(session_id, provider));
    }

    /// The right-pane toggle (⌘⌥B, and the header's PanelRight button):
    /// flips the open state, persists it, notifies, and re-reads the pane's
    /// data off the render path when it ends up open. While the Settings
    /// page stands open this is ignored with no state change (B11b): the
    /// page hides the pane without writing its state, and a toggle would
    /// otherwise flip it — and its per-session record — invisibly.
    pub(crate) fn toggle_right(&mut self, cx: &mut Context<Self>) {
        if self.settings_open(cx) {
            return;
        }
        // A user toggle always animates, even when a restore armed the
        // snap and no frame has consumed it yet.
        self.right_pane.update(cx, |pane, _| pane.snap = false);
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::PageMenu, cx); });
        crate::layout::Layout::stage(cx, |layout| {
            layout.right_open = !layout.right_open;
            // A pane that opens on the terminal takes it from the dock.
            layout.claim_terminal();
        });
        crate::baaz_log!("toggle right pane: open={}", crate::layout::Layout::get(cx).right_open);
        self.arm_browser_url_focus(cx);
        crate::layout::Layout::save(cx);
        // B7: the webview is prepared here, on the toggle — never as a
        // render side effect — so the opening animation's first frame
        // does not pay the construction cost.
        self.prewarm_browser_if_needed(cx);
        // B7fix2: the real webview rides a deferred task with the window
        // (no-op in tests/captures, or when the click path already built
        // it synchronously below).
        self.defer_browser_prewarm(cx);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
    }

    /// Switch between Sessions and Browse (⌥⌘B, the sidebar switch, the
    /// rail toggle, the View menu): whatever the window shows now, show
    /// the other half.
    pub(crate) fn toggle_browse_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Layout::get(cx).mode.other(), window, cx);
    }

    /// ⌘T, at window level so it fires with a page focused (B2a-H3): a
    /// person tab in the current project — from Sessions mode it
    /// switches to Browse first, then opens there. Sessions keep ⌘N;
    /// ⌘T never leaves the browser.
    pub(crate) fn new_browser_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != layout::Mode::Browse {
            self.set_mode(layout::Mode::Browse, window, cx);
        }
        self.new_browse_sidebar_tab(window, cx);
    }

    /// Set the window's mode, idempotently: asking for the current mode
    /// sets nothing (in Browse it still ensures the blank tab, so
    /// `mode:browse` twice never strands an empty centre) and never
    /// toggles. The open session, the sidebar's scroll and the shown tab
    /// are single states that a switch leaves alone. Browse's right pane
    /// stands closed: the Sessions pane waits aside and comes back
    /// ([`layout::Layout::enter_browse`]).
    pub(crate) fn set_mode(&mut self, mode: layout::Mode, window: &mut Window, cx: &mut Context<Self>) {
        if layout::Layout::get(cx).mode == mode {
            if mode == layout::Mode::Browse {
                self.ensure_browse_tab(window, cx);
            }
            return;
        }
        layout::Layout::stage(cx, |layout| layout.mode = mode);
        // One sidebar for both modes (owner, 2026-10-12): the switch lives
        // in the chrome row, so a mode change never opens or closes the
        // column on its own.
        // The switch leaves the address field (its dropdown belongs to
        // Browse's centre) and resets the page-holds edge.
        // `leave_browse_address` skips its refocus off Browse.
        self.browser.update(cx, |browser, _| browser.browse_last_holds = false);
        self.leave_browse_address(window, cx);
        // A menu belongs to the mode it opened in; left open it would hide a
        // page. (A modal follows the mode: its entry's host is `Modal`.)
        self.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
        // The CEF claim check filters `BaazBrowse` rows by this flag, set
        // here (never read from gpui state inside the router callback).
        crate::keymap::set_claim_live_browse(mode == layout::Mode::Browse);
        layout::Layout::update(cx, |layout| match mode {
            layout::Mode::Browse => layout.enter_browse(),
            layout::Mode::Sessions => layout.leave_browse(),
        });
        // The sidebar's new rows fade in (the pane asks for its own frames
        // while the fade runs).
        let at = cx.background_executor().now();
        self.sidebar_pane.update(cx, |pane, _| pane.start_mode_fade(at));
        if mode == layout::Mode::Browse {
            self.ensure_browse_tab(window, cx);
        }
        self.prewarm_browser_if_needed(cx);
        self.refresh_right_now(cx);
    }

    /// Open the right pane on `kind`: records the kind, forces the pane
    /// open, persists, notifies. Task T3 calls this from the ⌘K palette, so
    /// it works with no session open and never touches [`Self::active`].
    /// Calling it with the kind already showing while the pane is open
    /// closes the pane again, the way pressing a menu's own button closes it.
    /// Either way the pane's data is re-read off the render path when it ends
    /// up open. While the Settings page stands open this is ignored with no
    /// state change (B11b), like [`Self::toggle_right`]. Prefer
    /// [`Self::select_right`]: this stays only for the menu-toggle behaviour
    /// its test pins, so it compiles into test builds only.
    #[cfg(test)]
    pub(crate) fn show_right(&mut self, kind: layout::RightKind, cx: &mut Context<Self>) {
        if self.settings_open(cx) {
            return;
        }
        // A user pick always animates (see `toggle_right`).
        self.right_pane.update(cx, |pane, _| pane.snap = false);
        if crate::layout::Layout::get(cx).right_open && crate::layout::Layout::get(cx).right_kind == Some(kind) {
            crate::layout::Layout::stage(cx, |layout| layout.right_open = false);
        } else {
            crate::layout::Layout::stage(cx, |layout| layout.open_top(kind));
        }
        crate::baaz_log!("show right pane: {kind:?} open={}", crate::layout::Layout::get(cx).right_open);
        self.arm_browser_url_focus(cx);
        crate::layout::Layout::save(cx);
        // B7: prepare the webview on the pick, before the pane animates.
        self.prewarm_browser_if_needed(cx);
        // B7fix2: the real webview rides a deferred task with the window.
        self.defer_browser_prewarm(cx);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
    }

    /// Open the right pane on `kind` without ever closing it (B8): records
    /// the kind, forces the pane open, persists, notifies, and re-reads the
    /// pane's data off the render path. Asking for the kind already showing
    /// keeps the pane open on that kind — what makes this safe for header
    /// tab clicks, where toggling would punish the active tab. A legacy
    /// `diff`/`git` kind is stored as-is and reads back as the merged
    /// Changes view through [`layout::right_kind`]. Works with no session
    /// open and never touches [`Self::active`].
    pub(crate) fn select_right(&mut self, kind: layout::RightKind, cx: &mut Context<Self>) {
        // A user pick always animates (see `toggle_right`).
        self.right_pane.update(cx, |pane, _| pane.snap = false);
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::PageMenu, cx); });
        self.open_right_on(kind, cx);
        self.arm_browser_url_focus(cx);
    }

    /// The non-toggling open behind [`Self::select_right`] and the agent's
    /// `browser_navigate`: set the kind, force the pane open,
    /// persist, prewarm, save, refresh, notify. Never arms URL focus — the
    /// callers that owe it (a person's own open) arm it themselves, so an
    /// agent navigation never steals the keyboard.
    pub(crate) fn open_right_on(&mut self, kind: layout::RightKind, cx: &mut Context<Self>) {
        // The lower pane closes when the top takes its kind; the terminal
        // moves into the pane when that is what it shows (docs/82 8.1).
        crate::layout::Layout::stage(cx, |layout| layout.open_top(kind));
        crate::baaz_log!("open right pane: {kind:?} open={}", crate::layout::Layout::get(cx).right_open);
        crate::layout::Layout::save(cx);
        // B7: prepare the webview on the pick, before the pane animates.
        self.prewarm_browser_if_needed(cx);
        // B7fix2: the real webview rides a deferred task with the window.
        self.defer_browser_prewarm(cx);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
    }

    /// Arm the URL-field focus the next Browser frame owes (Z7a2): only a
    /// person's own open — a click, the palette, a shortcut — earns it, and
    /// only landing open on Browser. Restores never arm it, so a restored
    /// Browser pane leaves the keyboard where activation put it.
    fn arm_browser_url_focus(&mut self, cx: &mut App) {
        let layout = layout::Layout::get(cx);
        if layout.right_open && layout::right_kind(layout) == layout::RightKind::Browser {
            self.browser.update(cx, |browser, _| browser.browser_url_focus_armed = true);
        }
    }

    /// Persist one session's right-pane state without the session-list
    /// settle (B7): the in-memory override is edited at once, and the
    /// session row's write follows debounced off the UI thread. No
    /// `rejoin`, no `rebuild_search_index` — pane state is not searchable —
    /// and no synchronous store write on the gesture. Callers notify
    /// themselves; this only arms the write.
    pub(crate) fn store_right_state_cheap(
        &mut self,
        session_id: &str,
        state: crate::sessions::RightState,
        cx: &mut Context<Self>,
    ) {
        self.session_list.update(cx, |list, _| list.overrides.entry(session_id.to_owned()).or_default().right = Some(state));
        self.schedule_overrides_write(cx);
    }

    /// Persist one session's browser URL without the session-list settle
    /// (B7): same cheap debounced write as
    /// [`Self::store_right_state_cheap`], preserving every other right-pane
    /// field. A no-op when the stored URL already matches.
    pub(crate) fn remember_browser_url_cheap(
        &mut self,
        session_id: &str,
        url: String,
        cx: &mut Context<Self>,
    ) {
        // Q4c: while a provider switch is pending, every navigation sync
        // is deferred — the pane is neutral and describes neither session,
        // so persisting now would write pane activity onto a session the
        // pane is not showing. The live webviews keep their URLs, so the
        // next sync after the switch lands persists them then.
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
        let changed = self.session_list.update(cx, |list, _| {
            let entry = list.overrides.entry(session_id.to_owned()).or_default();
            let mut right = entry.right.clone().unwrap_or_default();
            if right.browser_url.as_deref() == Some(url.as_str()) {
                return false;
            }
            right.browser_url = Some(url);
            entry.right = Some(right);
            true
        });
        if changed {
            self.schedule_overrides_write(cx);
        }
    }

    /// The cheap right-pane persists' write: a snapshot queued for the
    /// session rows after a short delay, so a burst of toggles, expansions
    /// and navigations coalesces into one write and a toggle never blocks
    /// its own animation frame on the disk. Newest wins: a rename, pin or
    /// archive saved inside the window replaces this snapshot
    /// ([`crate::sessions::save_soon`]).
    pub(crate) fn schedule_overrides_write(&mut self, cx: &mut Context<Self>) {
        crate::sessions::save_soon(&self.session_list.read(cx).overrides);
    }

    /// Land every queued session-store write now (B7fix, B7fix2): on a
    /// session switch, so the outgoing session's pane state is on disk
    /// before the incoming one's is restored, and at quit and window
    /// close (`QuitApp`, the red-dot close and the app-quit hook), so no
    /// queued pane state or provider record dies with the process. Blocks
    /// only while a write is queued; with nothing queued it returns at once.
    pub(crate) fn flush_stores(&mut self) {
        crate::provider_sessions::flush();
        crate::sessions::flush();
    }

    /// Build the Browser webview outside `render` after an open action
    /// (B7fix2): the action itself — `toggle_right`, `show_right`, the
    /// palette's window command, the agent open — holds no window, so the
    /// construction rides a deferred task that runs at the end of the
    /// effect cycle, before the next frame paints, with the window in
    /// hand. Everything is re-checked at run time: the pane must still be
    /// open on Browser, the webview must still be missing (one per
    /// session, and a synchronously prewarmed click path cancels this
    /// into a no-op). Tests and captures never defer — the scripted page
    /// is already built synchronously by the windowless prewarm.
    pub(crate) fn defer_browser_prewarm(&mut self, cx: &mut Context<Self>) {
        // B2a-H2b: no pane page in Browse, so no deferred prewarm for
        // one either.
        if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
            return;
        }
        if !crate::layout::Layout::get(cx).right_open || layout::right_kind(crate::layout::Layout::get(cx)) != layout::RightKind::Browser {
            return;
        }
        if self.browser.read(cx).fake || cfg!(test) {
            return;
        }
        if self.existing_browser(cx).is_some() {
            return;
        }
        let harness = cx.entity();
        cx.defer(move |cx| {
            for window in cx.windows() {
                let warmed = window
                    .downcast::<Root>()
                    .and_then(|handle| {
                        handle
                            .update(cx, |root, window, cx| {
                                if let Ok(view) = root.view().clone().downcast::<Harness>() {
                                    if view == harness {
                                        view.update(cx, |this, cx| this.prewarm_browser_in(window, cx));
                                        return true;
                                    }
                                }
                                false
                            })
                            .ok()
                    })
                    .unwrap_or(false);
                if warmed {
                    break;
                }
            }
        });
    }

    /// Save the live pane state onto the active session (Z2): open, kind,
    /// and the Files preview/selection/expansion for the current project.
    /// Called on every user change while a session is active — toggle,
    /// show, close, preview, select, expand/collapse, the matching `--steps`
    /// verbs. With no session active (home/empty state) this is a no-op and
    /// today's global `layout.json` behaviour stands. `right_width` stays
    /// global and is never saved here.
    ///
    /// B7: the persist is the cheap debounced write above — no `rejoin`,
    /// no search rebuild, no synchronous store write.
    ///
    /// Q4c: while a provider switch is pending the pane is neutral — it
    /// describes neither session — so no gesture persists onto either
    /// record. The destination's pane restores when it lands, the previous
    /// session's on cancel. Every pane gesture funnels through here.
    pub(crate) fn save_right_for_active(&mut self, cx: &mut Context<Self>) {
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
        let Some(session_id) =
            self.active.clone().map(|view| view.read(cx).rt(cx).session_id.clone())
        else {
            return;
        };
        let (files_preview, files_selected, files_expanded) = match self.right_project(cx) {
            Some((root, _)) => self.right_pane.read(cx).files_state(&root),
            None => (None, None, Vec::new()),
        };
        // Z7a: the session's last browser URL rides along untouched — the
        // navigation sync owns it, and a pane toggle must never clear it.
        let browser_url = self
            .session_list.read(cx).overrides
            .get(&session_id)
            .and_then(|meta| meta.right.clone())
            .and_then(|right| right.browser_url);
        let live = crate::layout::Layout::get(cx);
        let state = crate::sessions::RightState {
            open: live.right_open,
            kind: live.right_kind.unwrap_or(layout::RightKind::Files),
            files_preview,
            files_selected,
            files_expanded,
            browser_url,
            lower: live.right_lower,
            split: live.right_split,
        };
        self.store_right_state_cheap(&session_id, state, cx);
    }

    /// Restore the live pane state from `session_id`'s stored [`RightState`]
    /// (Z2): what a session view's activation calls, so every switch — click,
    /// resume, reopen, provider reopen, handoff landing, launch restore —
    /// shows the pane exactly as that session left it. A session with no
    /// stored state shows the pane closed, as does a brand-new session. The
    /// Files preview/selection/expansion restore onto the current project
    /// root (the tree listing/diff/git caches stay per root), and the pane's
    /// data refreshes. Flipping open/closed arms the pane's snap
    /// ([`crate::right_pane::RightPane::snap`]) so the next frame lands with
    /// no animation; user toggles never arm it.
    pub(crate) fn restore_right_for_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        // B7fix: the outgoing session's debounced pane state lands on disk
        // before the incoming session's loads — a switch is the durability
        // point the toggle gesture itself must not pay for.
        self.flush_stores();
        let stored = self.session_list.read(cx).overrides.get(session_id).and_then(|meta| meta.right.clone());
        let open = stored.as_ref().is_some_and(|state| state.open);
        if crate::layout::Layout::get(cx).right_open != open {
            crate::layout::Layout::stage(cx, |layout| layout.right_open = open);
            self.right_pane.update(cx, |pane, _| pane.snap = true);
        }
        // The lower pane and its divider are the session's too (docs/82
        // 8.1): a session that stored none shows none. A pane that comes
        // back showing the terminal takes it from the dock.
        let (kind, lower, split) = match &stored {
            Some(state) => (Some(state.kind), state.lower, state.split),
            None => (None, None, None),
        };
        crate::layout::Layout::stage(cx, |layout| {
            if kind.is_some() {
                layout.right_kind = kind;
            }
            layout.right_lower = lower;
            layout.right_split = split.map(crate::layout::clamp_split);
            layout.claim_terminal();
        });
        if let Some((root, _)) = self.right_project(cx) {
            self.with_right_pane(cx, |pane, cx| pane.restore_files(&root, stored.as_ref(), cx));
        }
        // B7fix2: a switch that lands open on Browser builds the webview on
        // a deferred task with the window — never as a render side effect.
        self.defer_browser_prewarm(cx);
        self.refresh_right_now(cx);
    }

    /// The project the right pane reads for: root plus display name, from the
    /// in-memory current project — never the projects store on disk, which the
    /// old render path re-read on every frame. A session that runs in one of
    /// Baaz's worktrees reads its worktree instead (docs/82 6.3: its
    /// changes and its checkpoints are there, not in the project's folder),
    /// under the project's name. Pure: no disk.
    pub(crate) fn right_project(&self, cx: &gpui::App) -> Option<(std::path::PathBuf, String)> {
        let project = self.current_project(cx).map(|project| (project.root.clone(), project.name.clone()))?;
        let worktree = self.active.as_ref().map(|view| view.read(cx).rt(cx).workspace_path().to_owned()).filter(|folder| Self::in_worktree(folder));
        Some(match worktree {
            Some(folder) => (std::path::PathBuf::from(folder), project.1),
            None => project,
        })
    }

    /// A new terminal tab on the current project, showing the terminal
    /// where it sits for it ([`Self::show_terminal`],
    /// [`terminal::TerminalDock::new_terminal`]): the palette's "New
    /// terminal" and the composer's `/new-terminal`. Ignored while the
    /// Settings page stands open (B11b), like the dock toggle: opening the
    /// terminal would flip hidden state invisibly.
    pub(crate) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open(cx) {
            return;
        }
        self.show_terminal(cx);
        self.with_terminal_dock(cx, |dock, cx| dock.new_terminal(window, cx));
    }

    /// D51 "Open terminal" on a terminal tool card
    /// ([`terminal::TerminalDock::open_tab`]); ignored while the Settings
    /// page stands open (B11b), like the dock toggle.
    #[cfg(test)]
    pub(crate) fn open_terminal_tab(
        &mut self,
        tab: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_open(cx) {
            return;
        }
        self.show_terminal(cx);
        self.with_terminal_dock(cx, |dock, cx| dock.open_tab(tab, window, cx));
    }

    #[cfg(feature = "probe")]
    /// The `--steps` verb's route: show the terminal where it sits over a
    /// FakePty-backed tab and drain its script at once, so the capture
    /// replays the same bytes on every run. The payload names the tab;
    /// empty is "terminal". Fails the step while the Settings page stands
    /// open.
    pub(crate) fn step_terminal_dock(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let title = rest.trim();
        let title = if title.is_empty() { "terminal".to_owned() } else { title.to_owned() };
        let root = self
            .current_project(cx)
            .map(|project| project.root.clone())
            .unwrap_or_else(|| PathBuf::from(self.workspace(cx)));
        // The scripted dock stays shut while the Settings page stands open
        // (B11b): a capture aid must not flip hidden state either.
        if self.settings_open(cx) {
            return Err("the Settings page is open".into());
        }
        self.show_terminal(cx);
        self.with_terminal_dock(cx, |dock, cx| dock.open_scripted(&root, title, window, cx));
        Ok(())
    }

    /// The terminal tab host the dock draws, shared with the session views
    /// that run commands in it and the quit check.
    pub(crate) fn terminal_host(&self, cx: &App) -> Entity<TerminalHost> {
        self.terminal_dock.read(cx).host().clone()
    }

    /// What the dock needs from the root now (see [`terminal::DockFacts`]):
    /// the current project, the active session, and the durable provider
    /// records for the origins of the project's tabs.
    pub(crate) fn dock_facts(&self, cx: &App) -> terminal::DockFacts {
        let project = self.current_project(cx).map(|project| project.root.clone());
        let origin = self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone());
        let mut records = HashMap::new();
        if let Some(root) = &project {
            for tab in self.terminal_dock.read(cx).host().read(cx).tabs_for(root) {
                let record = tab.origin_session.as_ref().and_then(|session| Some((session, self.session_list.read(cx).provider_sessions.get(session)?)));
                if let Some((session, record)) = record {
                    records.insert(session.clone(), record.provider.clone());
                }
            }
        }
        terminal::DockFacts { project, origin, records }
    }

    /// Run `f` on the dock with fresh facts: every root call into the dock
    /// goes through here, so the dock never acts on a stale project or
    /// session.
    pub(crate) fn with_terminal_dock<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut terminal::TerminalDock, &mut Context<terminal::TerminalDock>) -> R,
    ) -> R {
        let facts = self.dock_facts(cx);
        self.terminal_dock.update(cx, |dock, cx| {
            dock.facts = facts;
            f(dock, cx)
        })
    }

    /// The agent mark for a terminal tab whose origin is `origin`: the
    /// root's durable record first, then the dock's bridge map
    /// ([`terminal::TerminalDock::tab_provider`]).
    #[cfg(test)]
    pub(crate) fn terminal_tab_provider(&self, origin: Option<&str>, cx: &App) -> Option<aui_icons::Provider> {
        let record = origin.and_then(|session| self.session_list.read(cx).provider_sessions.get(session)).map(|record| record.provider.as_str());
        self.terminal_dock.read(cx).tab_provider(origin, record)
    }

    /// The dock for this frame: push the facts, then compose its view. The
    /// view draws nothing once the dock has settled shut. Composed
    /// uncached: the facts are pushed during the root's render.
    fn compose_terminal_dock(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let facts = self.dock_facts(cx);
        self.terminal_dock.update(cx, |dock, _| dock.facts = facts);
        self.terminal_dock.clone().into_any_element()
    }

    /// One event from the dock, mapped to the root's own method.
    fn on_terminal_dock_event(
        &mut self,
        _dock: &Entity<terminal::TerminalDock>,
        event: &terminal::TerminalDockEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            terminal::TerminalDockEvent::OpenUrl(url) => self.open_url_in_browser(url.clone(), cx),
            terminal::TerminalDockEvent::ConfirmClose { tab, running } => {
                if let Some(dialog) = Self::close_tab_dialog(tab, Some(running)) {
                    self.set_dialog(cx, dialog);
                }
            }
            terminal::TerminalDockEvent::Move(place) => self.move_terminal(*place, window, cx),
            // `set_draft` only: the draft is never sent from here. An
            // occupied composer is appended to, never clobbered. The visible
            // composer takes it; with none on screen (Browse, Settings),
            // "Add to which session?" (docs/82 8.2).
            terminal::TerminalDockEvent::Ask { command, output } => {
                let ask = crate::browser::Parcel::TerminalOutput { command: command.clone(), output: output.clone() };
                self.send_parcel(ask, window, cx);
            }
        }
    }

    /// The transcript row's pane toggles (D6, docs/43 §2.2): Changes ·
    /// Files · Page, 28 px ghost buttons; the one the pane shows is lit and
    /// announces itself pressed; none is lit while the pane is closed. A
    /// click opens the pane on that screen (sliding the strip when it was
    /// open on another), and the lit toggle closes it.
    fn pane_toggles(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = crate::layout::Layout::get(cx).right_open && !self.settings_open(cx);
        let shown = layout::right_kind(crate::layout::Layout::get(cx));
        let mut row = h_flex().flex_none().items_center().gap(px(ROW_GAP));
        for (kind, id) in [
            (layout::RightKind::Changes, "hd-centre-pane-changes"),
            (layout::RightKind::Files, "hd-centre-pane-files"),
            (layout::RightKind::Browser, "hd-centre-pane-page"),
        ] {
            let (glyph, label) = (right::screen_glyph(kind), kind.label());
            let lit = open && shown == kind;
            let click = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.pane_toggle_click(kind, cx);
                if kind == layout::RightKind::Browser && crate::layout::Layout::get(cx).right_open {
                    // B7fix2: the click builds the real webview itself, with
                    // the window, ahead of the strip's first frame.
                    this.prewarm_browser_in(window, cx);
                }
            });
            row = row.child(aui::workbench::pane_toggle(id, glyph, label, lit).on_click(click));
        }
        row.into_any_element()
    }

    /// A pane toggle's click: the lit one closes the pane; any other opens
    /// the pane on its screen (the strip slides there when it was open).
    pub(crate) fn pane_toggle_click(&mut self, kind: layout::RightKind, cx: &mut Context<Self>) {
        let lit = crate::layout::Layout::get(cx).right_open && layout::right_kind(crate::layout::Layout::get(cx)) == kind;
        if lit {
            self.toggle_right(cx);
        } else {
            self.select_right(kind, cx);
        }
    }




    /// The address field's live input with its omnibox keys (B2a-H4), as
    /// the new-tab field draws it; the Field panel (`field_panel`) draws the
    /// same input in its own query row — only one of them renders per frame. ↑/↓ move the highlight, Tab completes,
    /// Escape restores; plain Enter commits and ⌘↵ asks, both through the
    /// input's own PressEnter (⌘↵ rides the input context's
    /// `secondary-enter` binding, so it never also fires the commit).
    fn browse_address_input(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(input) = self.browser.read(cx).browse_address.clone() else {
            return div().into_any_element();
        };
        let keys = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, window, cx| {
            match event.keystroke.key.as_str() {
                "up" => this.move_omnibox(-1, cx),
                "down" => this.move_omnibox(1, cx),
                "tab" => this.complete_omnibox(window, cx),
                "escape" => this.leave_browse_address(window, cx),
                _ => {}
            }
        });
        div()
            .id("hd-browse-address-keys")
            .flex_1()
            .min_w_0()
            .on_key_down(keys)
            .child(
                a11y_text_input("browse-address-field", "Address and search", &input, cx)
                    .flex_1()
                    .min_w_0(),
            )
            .into_any_element()
    }

    /// A small chrome button (B2a-H5): the library's small ghost button,
    /// named for assistive tech, with its click. Find-strip,
    /// permission-strip and popover rows share it.
    fn browse_chrome_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &str,
        aria: &str,
        click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        _cx: &mut Context<Self>,
    ) -> aui::data::Button {
        button(id, label.to_owned()).ghost().sm().accessibility_label(aria.to_owned()).on_click(click)
    }

    /// One ⋯/popover menu row (B2a-H5): label plus an optional shortcut
    /// hint on the menu row's height, a MenuItem named for assistive tech,
    /// painting its hover with the one model.
    fn browse_menu_row(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &str,
        hint: Option<&str>,
        click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> aui::interact::Interactive {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let content = h_flex()
            .w_full()
            .h(px(MENU_ROW_H))
            .px(metrics.item_px)
            .gap(metrics.gap)
            .child(div().flex_1().min_w_0().truncate().text_role(TextRole::Body).child(label.to_owned()))
            // Shortcut hints: `t-micro` in `ink-4` (docs/83 §7).
            .children(hint.map(|hint| div().flex_none().role_in(TextRole::Micro, cx).text_color(p.ink_4).child(hint.to_owned())));
        interactive(id, content).role("menuitem").label(label.to_owned()).on_click(click)
    }

    /// A Browse chrome popover (B2a-H5): the one floating panel, its rows
    /// in a column. Absolute under the toolbar, over the page (which the
    /// covering rule obscures while it stands). Downloads, extensions and ⋯
    /// share it.
    fn browse_popover(&self, id: impl Into<gpui::ElementId>, aria: &str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        // In the overlay window (the footer's popovers, R4) it sits in flow,
        // so the window can put it right above its button; in the main
        // window it hangs from the top-right of the page.
        let hosted = self.hosting_overlay;
        menu_panel(gpui::Empty, cx)
            .id(id)
            .when(!hosted, |d| d.absolute().top(px(scale::SP_2)).right(px(scale::SP_3)))
            .w(px(BROWSE_POPOVER_W))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(ROW_GAP))
            .role(gpui::Role::Menu)
            .aria_label(aria.to_owned())
    }


    /// The find strip (B2a-H5): a row above the page — a styled query
    /// field, "3 / 12", ↑/↓, a Match case chip, Done. Only the shown
    /// tab's open strip renders, never on a crashed page; the input value
    /// is set when the strip opens or the tab switches — never here, so
    /// render mutates nothing. Esc closes (the wrapper owns it, like the
    /// address field); Enter steps through the input's own PressEnter and
    /// Shift+Enter steps back.
    fn render_browse_find(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::browser::chrome::find_count_text;
        let shown = self.browser.read(cx).browse_centre_shown(cx);
        let (query, match_case) = match self.browser.read(cx).find_strips.visible(shown) {
            Some((_, strip)) => (strip.query.clone(), strip.match_case),
            None => return None,
        };
        if self.browse_centre_state(cx).is_some_and(|state| state.read(cx).crashed().is_some()) {
            return None;
        }
        let input = self.browser.read(cx).browse_find.clone()?;
        let p = cx.aui().colors;
        let count = self
            .browse_centre_state(cx)
            .and_then(|state| state.read(cx).find_state())
            .filter(|found| found.query == query)
            .map(|found| (find_count_text(found.active, found.total), found.active, found.total));
        let keys = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, window, cx| {
            if event.keystroke.key.as_str() == "escape" {
                this.close_browse_find(window, cx);
            }
        });
        // Shift+Enter steps back in the capture phase and stops there, so
        // the input never also sees it and emits its own forward PressEnter.
        let back = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, _window, cx| {
            if event.keystroke.key.as_str() == "enter" && event.keystroke.modifiers.shift {
                this.find_next(false, cx);
                cx.stop_propagation();
            }
        });
        let mut row = h_flex()
            .id("browse-find")
            .w_full()
            .flex_none()
            .items_center()
            .h(px(scale::H_XL))
            .px(px(scale::SP_3))
            .gap(px(scale::SP_2))
            .bg(p.surface_1)
            .border_b_1()
            .border_color(p.line)
            .role(gpui::Role::Group)
            .aria_label("Find in page")
            .capture_key_down(back)
            .on_key_down(keys)
            .child(
                aui::data::field_frame("browse-find-frame", &input, window, cx)
                    .flex_1()
                    .min_w_0()
                    .gap(px(scale::SP_2))
                    .child(icon(IconName::Search).size(px(scale::ICON_SM)).color(p.fg_muted))
                    .child(
                        a11y_text_input("browse-find-field", "Find in page", &input, cx).flex_1().min_w_0(),
                    ),
            );
        match count {
            Some((text, active, total)) => {
                row = row.child(
                    div()
                        .id("browse-find-count")
                        .flex_none()
                        .ui(TextRole::Caption)
                        .text_color(p.fg_muted)
                        .role(gpui::Role::Label)
                        .aria_label(format!("{active} of {total} matches"))
                        .child(text),
                );
            }
            None => {
                row = row.child(
                    div()
                        .id("browse-find-count")
                        .flex_none()
                        .ui(TextRole::Caption)
                        .text_color(p.fg_muted)
                        .role(gpui::Role::Label)
                        .aria_label("No matches yet")
                        .child("…"),
                );
            }
        }
        let prev = button("browse-find-prev", "↑")
            .ghost()
            .sm()
            .accessibility_label("Previous match")
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.find_next(false, cx);
            }));
        let next = button("browse-find-next", "↓")
            .ghost()
            .sm()
            .accessibility_label("Next match")
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.find_next(true, cx);
            }));
        let case = button("browse-find-case", "Match case")
            .sm()
            .accessibility_label(if match_case { "Match case on" } else { "Match case off" })
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.toggle_browse_find_case(cx);
            }));
        let case = if match_case { case.primary() } else { case.ghost() };
        let done = button("browse-find-done", "Done").sm().on_click(cx.listener(
            |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.close_browse_find(window, cx);
            },
        ));
        row = row.child(prev).child(next).child(case).child(done);
        Some(row.into_any_element())
    }

    /// The permission strip (B2a-H5): a row above the page while the shown
    /// tab holds an unanswered ask — a site glyph, the host in bold, the
    /// ask, then Block and Always block (ghost), Allow once (secondary) and
    /// Always allow (primary). Asks with a kept answer never reach it (the
    /// page reconcile answers them), and a page dismissal drops it with no
    /// state here.
    fn render_browse_permission(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::browser::chrome::{permission_sentence, permission_want};
        let p = cx.aui().colors;
        let state = self.browse_centre_state(cx)?;
        let request = state.read(cx).permissions().first()?.clone();
        let sentence = permission_sentence(&request.origin, &request.kinds);
        let want = permission_want(&request.kinds);
        let id = request.id;
        let mut row = h_flex()
            .id("browse-permission")
            .w_full()
            .flex_none()
            .items_center()
            .h(px(scale::H_XL))
            .px(px(scale::SP_3))
            .gap(px(scale::SP_2))
            .bg(p.surface_1)
            .border_b_1()
            .border_color(p.line)
            .role(gpui::Role::Group)
            .aria_label("Permission request")
            .child(icon(IconName::Globe).size(px(scale::ICON_SM)).color(p.fg_muted))
            .child(
                div()
                    .id("browse-permission-host")
                    .flex_none()
                    .ui(TextRole::Body)
                    .semibold()
                    .text_color(p.ink)
                    // The host, as the toolbar shows it (userinfo and scheme
                    // dropped) — never a spoofable raw origin string.
                    .child({
                        let (_, host, _) = crate::browser::toolbar::split_host_path(&request.origin);
                        if host.is_empty() { request.origin.clone() } else { host }
                    }),
            )
            .child(
                div()
                    .id("browse-permission-text")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .ui(TextRole::Body)
                    .text_color(p.ink)
                    .role(gpui::Role::Label)
                    .aria_label(sentence.clone())
                    .child(format!("wants to {want}")),
            );
        use crate::browser::PermissionAnswer;
        let answer = |id_str: &'static str, label: &'static str, answer: PermissionAnswer, cx: &mut Context<Self>| {
            button(id_str, label).sm().on_click(cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.answer_shown_permission(id, answer, cx);
            }))
        };
        // This time, or for good on this site (docs/82 8.6): the "always"
        // answers are kept and Settings › Browser › Site permissions lists
        // them.
        let block = answer("browse-permission-block", "Block", PermissionAnswer::Block, cx).ghost();
        let never = answer("browse-permission-always-block", "Always block", PermissionAnswer::AlwaysBlock, cx).ghost();
        let once = answer("browse-permission-once", "Allow once", PermissionAnswer::AllowOnce, cx);
        let always = answer("browse-permission-always-allow", "Always allow", PermissionAnswer::AlwaysAllow, cx).primary();
        row = row.child(block).child(never).child(once).child(always);
        Some(row.into_any_element())
    }

    /// The Baaz crashed page (B2a-H5): gpui in the page rect while the
    /// native view hides under it. The library reports no load-error
    /// event — only `Crashed` — so every Baaz error page renders from
    /// that state, with Reload navigating the tab again at its own URL.
    fn render_browse_crashed(&self, reason: String, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let reload = self.browse_chrome_button(
            "browse-crashed-reload",
            "Reload",
            "Reload page",
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.reload_browse_shown(window, cx);
            }),
            cx,
        );
        v_flex()
            .size_full()
            .id("browse-crashed")
            .items_center()
            .justify_center()
            .gap(px(scale::SP_3))
            .role(gpui::Role::Group)
            .aria_label("This page stopped")
            .child(div().ui(TextRole::Subtitle).text_color(p.ink).child("This page stopped"))
            .child(div().ui(TextRole::Caption).text_color(p.fg_muted).child(reason))
            .child(reload)
            .into_any_element()
    }

    /// The downloads popover (B2a-H5): every download's name, its
    /// size-and-ETA line (or Done/Cancelled/Failed), the session credit on
    /// agent-saved files — with Reveal in Finder on landed files, Cancel
    /// download while active, Try again on failed ones — plus All
    /// downloads for `~/Downloads`.
    pub(crate) fn render_browse_downloads(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::browser::chrome::{download_line, download_owner_label};
        if !self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::Downloads) {
            return None;
        }
        let p = cx.aui().colors;
        let downloads = self.browser.read(cx).browse_downloads(cx);
        let now = crate::browser::tabs::now_ms();
        let mut menu = self.browse_popover("browse-downloads", "Downloads", cx);
        if downloads.is_empty() {
            // R4: the button is always in the footer; an empty list says so.
            menu = menu.child(
                div().w_full().px(cx.aui().metrics.item_px).py(px(scale::SP_2)).ui(TextRole::Body).text_color(p.fg_muted).child("No downloads yet."),
            );
            let folder = self.browse_menu_row(
                "browse-downloads-folder",
                "Open Downloads folder",
                None,
                cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    this.dismiss_browse_popovers(cx);
                    let _ = crate::system_open::open_detached([crate::layout::browser_download_dir(crate::layout::Layout::get(cx))]);
                }),
                cx,
            );
            return Some(menu.child(folder).into_any_element());
        }
        for (tab, owner, download) in &downloads {
            let tab = *tab;
            let id = download.id;
            let started = self.browser.read(cx).download_started.get(&(tab, id)).copied().unwrap_or(now);
            let elapsed = now.saturating_sub(started) / 1000;
            let line = download_line(download, elapsed);
            let credit = match owner {
                crate::browser::Owner::Session(session) => {
                    download_owner_label(owner, Some(&self.session_display_name(session, cx)))
                }
                crate::browser::Owner::Person => None,
            };
            let detail = match credit {
                Some(credit) => format!("{line} · {credit}"),
                None => line,
            };
            let mut item = v_flex()
                .w_full()
                .flex_none()
                .gap(px(scale::SP_1))
                .px(cx.aui().metrics.item_px)
                .py(px(scale::SP_2))
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .ui(TextRole::Body)
                        .text_color(p.ink)
                        .child(download.file_name.clone()),
                )
                .child(
                    div()
                        .id(("browse-download-line", id))
                        .ui(TextRole::Micro)
                        .text_color(p.fg_muted)
                        .role(gpui::Role::Label)
                        .aria_label(detail.clone())
                        .child(detail),
                );
            let actions: Option<AnyElement> = match download.state {
                aui_webview::DownloadStatus::Downloading => {
                    let cancel = self.browse_chrome_button(
                        ("browse-download-cancel", id),
                        "Cancel download",
                        "Cancel download",
                        cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            this.cancel_browse_download(tab, id, cx);
                        }),
                        cx,
                    );
                    Some(div().flex().flex_row().child(cancel).into_any_element())
                }
                aui_webview::DownloadStatus::Complete => {
                    let path = download.path.clone();
                    let reveal = self.browse_chrome_button(
                        ("browse-download-reveal", id),
                        "Reveal in Finder",
                        "Reveal in Finder",
                        cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            let path = path.clone();
                            this.reveal_browse_download(&path, cx);
                        }),
                        cx,
                    );
                    Some(div().flex().flex_row().child(reveal).into_any_element())
                }
                aui_webview::DownloadStatus::Failed => {
                    let url = download.url.clone();
                    let retry = self.browse_chrome_button(
                        ("browse-download-retry", id),
                        "Try again",
                        "Try again",
                        self.main_listener(cx, move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                            let url = url.clone();
                            this.retry_browse_download(tab, &url, window, cx);
                        }),
                        cx,
                    );
                    Some(div().flex().flex_row().child(retry).into_any_element())
                }
                aui_webview::DownloadStatus::Cancelled => None,
            };
            if let Some(actions) = actions {
                item = item.child(actions);
            }
            menu = menu.child(item);
        }
        let all = self.browse_menu_row(
            "browse-downloads-all",
            "All downloads",
            None,
            |_, _, _| {
                crate::browser::BrowserView::open_downloads_folder();
            },
            cx,
        );
        menu = menu.child(all);
        // D4: opened from the sidebar's button, the popover hangs under it
        // (window coordinates), its left edge on the button's.
        if self.hosting_overlay {
            return Some(menu.relative().top_0().right_0().into_any_element());
        }
        if let Some(anchor) = self.browser.read(cx).downloads_anchor {
            let at = gpui::point(anchor.left(), anchor.bottom() + px(scale::SP_2));
            return Some(gpui::deferred(gpui::anchored().position(at).child(menu.relative().top_0().right_0())).with_priority(2).into_any_element());
        }
        Some(menu.into_any_element())
    }

    /// The extensions popover (B2a-H5): stock CEF exposes no extension
    /// list, so a fixed message — Manage opens chrome://extensions, the
    /// Store opens the Chrome Web Store, each in a new tab.
    pub(crate) fn render_browse_extensions(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::Extensions) {
            return None;
        }
        let p = cx.aui().colors;
        let mut menu = self.browse_popover("browse-extensions", "Extensions", cx);
        // R4 spike (11 Nov): Chromium here can list extensions but cannot run
        // their popups or pages, so Baaz does not offer installing them yet;
        // ad and tracker blocking is built in instead (board R4Daily 4c).
        menu = menu.child(
            div()
                .w_full()
                .px(cx.aui().metrics.item_px)
                .py(px(scale::SP_2))
                .role_in(TextRole::Body, cx)
                .text_color(p.fg_muted)
                .child("Chrome extensions don’t run in Baaz yet. Ads and trackers are blocked without one."),
        );
        let settings = self.browse_menu_row(
            "browse-extensions-adblock",
            "Ad blocking settings",
            None,
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.dismiss_browse_popovers(cx);
                this.open_browse_settings(cx);
            }),
            cx,
        );
        menu = menu.child(settings);
        let _ = window;
        Some(menu.into_any_element())
    }

    /// The bar (canvas "Baaz v3", chrome revised 2026-10-13): the sidebar
    /// toggle past the window's lights, the mode switch at the sidebar's
    /// edge, the title slot (the session title; in Browse, closed only, the
    /// page's way back, back / forward / reload and the address at rest) and
    /// the actions at the right (the session's actions; the toast moved to
    /// the corner stack in D4).
    /// The shell draws every slot at the same spot in both states.
    fn render_bar_chrome(&self, window: &mut Window, cx: &mut Context<Self>) -> BarChrome {
        let lead = self.render_sidebar_toggle(cx);
        let switch_to = cx.entity().downgrade();
        let shown = match crate::layout::Layout::get(cx).mode {
            layout::Mode::Browse => aui::screens::sidebar::Mode::Browse,
            _ => aui::screens::sidebar::Mode::Sessions,
        };
        let switch = aui::screens::sidebar::mode_switch("bar-modes", shown)
            .on_switch(move |mode, window, cx| {
                let target = match mode {
                    aui::screens::sidebar::Mode::Sessions => layout::Mode::Sessions,
                    aui::screens::sidebar::Mode::Browse => layout::Mode::Browse,
                };
                let _ = switch_to.update(cx, |this, cx| this.set_mode(target, window, cx));
            })
            .into_any_element();
        if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
            // Browse: the page controls and the address live in the sidebar's
            // own header; the centre is the page, so the row shows nothing.
            BarChrome { lead, switch: Some(switch), title: None, right: None }
        } else {
            BarChrome {
                lead,
                switch: Some(switch),
                title: Some(self.render_session_title(window, cx)),
                right: Some(self.session_header_actions(cx)),
            }
        }
    }

    /// Hands the shell's hover zones (the peek's corner) to the Browse page,
    /// so a pointer resting there over the native page still reaches the
    /// shell; none outside Browse.
    pub(crate) fn set_browse_hover_zones(&mut self, zones: Vec<gpui::Bounds<Pixels>>, cx: &mut Context<Self>) {
        let zones: Vec<[f32; 4]> = if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
            zones.iter().map(|b| [f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]).collect()
        } else {
            Vec::new()
        };
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _| state.set_hover_zones(zones));
        }
    }

    /// The bar's title slot in Sessions: the active session's label (or the
    /// rename field for the session or the project), semibold 13 — and, for
    /// a session in a worktree, its branch beside it, muted. The project is chosen from the sidebar and the Projects
    /// palette, never here.
    fn render_session_title(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let label = self.session_header_label(cx);
        // The session title leads the row: first child of the header cell,
        // so its left edge sits where the project crumb's did. The title
        // flexes inside the header cell and clips to one line, so a whole
        // first prompt as the derived title can never push the overflow
        // button out. There is no width token in aui-tokens, so the flex
        // leftover — not a fixed max — is the constraint, which also holds
        // on narrow windows.
        let mut title = h_flex()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .items_center()
            .gap(px(scale::SP_3))
            .text_color(p.ink)
            .ui(TextRole::Body)
            .semibold();
        // A project rename (from the project menu) swaps the lead for the
        // same dense field the sidebar row uses; the commit path is the
        // same `ConfirmRename`, Escape the same `cancel`.
        let renaming_project_here =
            self.current_project.as_deref().is_some_and(|id| self.renaming_project(cx).as_deref() == Some(id));
        if renaming_project_here {
            title = title.child(
                div().flex_1().min_w_0().overflow_hidden().child(self.rename_field(window, cx)),
            );
        } else if let Some(label) = label {
            // Renaming the open session swaps the session label for the same
            // dense field the sidebar row uses (Task A); the commit path is
            // the same `ConfirmRename`, Escape the same `cancel`.
            let renaming_here = self
                .active
                .as_ref()
                .map(|view| view.read(cx).rt(cx).session_id.clone())
                .is_some_and(|id| self.renaming(cx).as_deref() == Some(id.as_str()));
            if renaming_here {
                // The field's own root is `w_full`, so the flex item clips:
                // the overflow button keeps its slot instead of being pushed
                // out.
                title = title.child(
                    div().flex_1().min_w_0().overflow_hidden().child(self.rename_field(window, cx)),
                );
            } else if let Some(branch) = self.active_worktree_branch(cx) {
                // A worktree session names its branch beside its title, muted
                // and regular (docs/82 6.2, docs/83 §9); the title gives way
                // first, and the two sit together at the left.
                title = title
                    .child(div().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(p.fg_muted)
                            .font_weight(gpui::FontWeight::NORMAL)
                            .child(branch),
                    )
                    .child(div().flex_1());
            } else {
                title = title.child(div().flex_1().min_w_0().truncate().child(label));
            }
        }
        title.into_any_element()
    }

    /// The open session's branch when it runs in one of Baaz's worktrees
    /// (from the process cache: no database on a frame).
    fn active_worktree_branch(&self, cx: &gpui::App) -> Option<String> {
        let view = self.active.as_ref()?;
        baaz_core::worktrees::cached_branch_for_folder(std::path::Path::new(view.read(cx).rt(cx).workspace_path()))
    }

    /// The session header's title: the open session's chain head label, or
    /// "Skills" while the Skills page is open.
    pub(crate) fn session_header_label(&self, cx: &mut Context<Self>) -> Option<String> {
        // The click's target, from the list entry — never the view, so the
        // header answers on the click's own frame, before any page arrives.
        let target =
            self.open_state.pending_id.clone().or_else(|| self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone()));
        // One identity per chain: header, window and selection all resolve
        // the raw id to its head, and the collapsed view row names it —
        // the same label the sidebar's one row wears (Y2a, Y2a3).
        let target = target.map(|id| self.chain_head(&id, cx));
        // The Skills page names itself plainly while open: "Skills".
        if self.skills_open(cx) {
            Some(self.skills_page.read(cx).title(cx).to_string())
        } else {
            // A draft with no row yet (a Muse draft opened while Muse
            // starts, before any list) still names itself, as its row will.
            target.as_deref().and_then(|id| {
                self.collapsed_head_label(id, cx)
                    .or_else(|| self.open_state.names_draft(id).then(|| sidebar::UNNAMED.to_owned()))
            })
        }
    }

    /// The session header's actions — the right pane's views, terminal,
    /// the session menu — drawn in the centre column's header.
    fn session_header_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let overflow =
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.open_menu(MenuKind::Overflow, cx));
        let overflow_open = self.overlays.read(cx).is_open(MenuKind::Overflow);
        // The terminal toggle: ⌃`'s button, lit while the terminal shows,
        // at the bottom or in the pane.
        let term_toggle = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.toggle_terminal(window, cx);
        });
        // Lit like the pane's other views (docs/82 8.1: the terminal is one
        // of them now), wherever it shows.
        let term_lit = crate::layout::Layout::get(cx).terminal_shown();
        let term_button = aui::workbench::pane_toggle("hd-centre-terminal", right::screen_glyph(layout::RightKind::Terminal), "Terminal", term_lit)
            .on_click(term_toggle);
        // D6 (docs/43 §2.2): Changes · Files · Page choose the pane's screen
        // (lit = the screen it shows); then Terminal, a hairline, the menu.
        let toggles = self.pane_toggles(cx);
        // docs/82 8.4: the pages the open session owns, before the views —
        // its Page, or Browse on its task folder when it owns several.
        let pages = self.active_page_count(cx);
        let pages_button = (pages > 0).then(|| {
            let noun = if pages == 1 { "page" } else { "pages" };
            let open = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                if let Some(id) = this.active_id(cx) {
                    this.open_session_pages(id, window, cx);
                }
            });
            aui::data::button("hd-centre-pages", format!("{pages} {noun}"))
                .ghost()
                .muted()
                .xs()
                .accessibility_label(format!("Open this session's {pages} {noun}"))
                .on_click(open)
        });
        // The wrapper reports the overflow button's own rect (its only
        // child), which is what the overflow menu seats at.
        let overflow_report = cx.entity().downgrade();
        h_flex()
            .flex_none()
            .items_center()
            .gap(px(ROW_GAP))
            .children(pages_button)
            .child(toggles)
            .child(term_button)
            .child(div().flex_none().mx(px(scale::SP_2)).w_px().h(px(scale::ICON_DEFAULT)).bg(cx.aui().colors.line))
            .child(
                div()
                    .flex_none()
                    .on_children_prepainted(move |bounds, _, cx| {
                        if let Some(first) = bounds.first() {
                            let bounds = *first;
                            let _ = overflow_report.update(cx, |this, cx| {
                                this.overlays.update(cx, |overlays, cx| overlays.note_anchor(crate::overlays::Anchor::Overflow, bounds, cx));
                            });
                        }
                    })
                    .child(
                        icon_button("hd-centre-overflow", IconName::Dots)
                            .ghost()
                            .muted()
                            .accessibility_label("Open session menu")
                            .menu_open(overflow_open)
                            .on_click(overflow),
                    ),
            )
            .into_any_element()
    }

    /// The reconnect banner, above everything in the centre column.
    fn render_wire_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Muse's connection is Muse's business: a Claude Code or Codex
        // session (or no session) never wears its banner (docs/50 P1).
        let muse_session = self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).provider_kind().has(provider::ProviderTrait::SharedChild));
        if !muse_session {
            return None;
        }
        // The banner names the connection's own provider, read from its
        // descriptor (rule 14): it only ever draws on that provider's session.
        let name = crate::providers::shared_label();
        let (kind, text) = match &self.wire {
            Wire::Reconnecting => (BannerKind::Waiting, format!("{name} disconnected, reconnecting\u{2026}")),
            Wire::Connecting => (BannerKind::Waiting, format!("Starting {name}\u{2026}")),
            Wire::Down(reason) => (BannerKind::Error, format!("{name} is not running: {reason}")),
            Wire::Ready => return None,
        };
        Some(
            div()
                .w_full()
                .px(px(scale::SP_7))
                .pt(px(scale::SP_4))
                .child(banner("wire", kind, vec![BannerRun::Text(text.into())]))
                .into_any_element(),
        )
    }

    /// `--replay`: open one session out of a capture file, once, while the
    /// root is built (`Harness::new`), so frame one draws the folded rows.
    #[cfg(feature = "probe")]
    fn open_replay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.args.replay.take() else { return };
        // The open-at-length mark (docs/51 M): parse and fold the whole
        // capture, then the first frame that draws it.
        crate::log::trace_reset();
        crate::log::trace_mark("replay-open");
        let at_rest = self.still();
        // The capture's own root when it names one: the replayed row groups
        // where a live session of that root would.
        let workspace = sidebar::replay_workspace(&path)
            .map(|root| projects::canonical_str(&root))
            .unwrap_or_else(|| self.workspace(cx));
        // `--bench` through the shell drives the stream itself, event by
        // event, so the view opens in bench-replay state instead of folding
        // the capture at once.
        let bench = self.args.bench_shell;
        // Anything else folds through the registry: the provider whose own
        // recording it is reads it (docs/82 5.7), and the session is that
        // provider's — unless the flag already names it in its own words (a
        // route).
        let replayed = (!bench).then(|| std::fs::read_to_string(&path).map(|text| crate::providers::replay(&text)));
        let provider = match &replayed {
            Some(Ok(Some((id, _)))) if crate::providers::ProviderId::resolve(&self.args.provider).ok() != Some(*id) => {
                id.as_str().to_owned()
            }
            _ => self.args.provider.clone(),
        };
        let overlays = self.overlays.clone();
        let capture = self.capture.clone();
        // The capture names its own session; this id is a placeholder the view
        // replaces the moment the first line is folded.
        let view = cx.new(|cx| {
            let host = SessionHost { provider_id: provider, workspace, overlays, capture, terminal_host: None };
            let mut view = SessionView::new("replay".to_owned(), host, window, cx);
            view.set_at_rest(at_rest);
            let failed = |error: String, view: &mut SessionView, cx: &mut Context<SessionView>| {
                view.runtime().clone().update(cx, |rt, cx| rt.show_banner(&format!("{}: {error}", path.display()), cx));
            };
            match replayed {
                // The bench drives a Muse-format capture event by event.
                None => match provider_muse::replay::Capture::load(&path) {
                    Ok(capture) => {
                        let id = capture.session_id().unwrap_or_else(|| "bench".to_owned());
                        view.runtime().clone().update(cx, |rt, cx| rt.begin_bench_replay(id, capture.sent, cx));
                    }
                    Err(error) => failed(error.to_string(), &mut view, cx),
                },
                Some(Ok(Some((_, replay)))) => view.runtime().clone().update(cx, |rt, cx| rt.load_replay(replay, cx)),
                Some(Ok(None)) => failed("no registered provider reads this recording".to_owned(), &mut view, cx),
                Some(Err(error)) => failed(error.to_string(), &mut view, cx),
            }
            view.runtime().clone().update(cx, |rt, cx| rt.load_history(cx));
            view
        });
        crate::log::trace_mark("replay-folded");
        crate::log::trace_arm_first_frame();
        self.registry.resubscribe(Some(&view), cx);
        // The replayed row stands for the capture's own turns: without them
        // the empty filter reads it as a turn-less session and drops it the
        // moment it is not the open one — so switching sessions in a replay
        // window would shrink the list under the scroll (the click moved
        // `-1262 → -1198` on the clamp, not on the reveal).
        let mut replayed = SessionEntry::replayed(&view.read(cx).rt(cx).session_id, &path, &self.session_list.read(cx).projects);
        // A title Baaz already stored for this capture (a rename, or the
        // transcript-derived title an earlier open wrote) names the row the
        // way it names a live one. Without this only the first open in a
        // state dir showed the transcript's title: `title_from_transcript`
        // below sees the stored one and stops, and the row kept the file
        // name on every later open (the `jump-pill` capture flake).
        if let Some(label) = sidebar::label_ladder(self.session_list.read(cx).overrides.get(&replayed.id), None, None) {
            replayed.label = label.to_owned();
        }
        replayed.named = self.session_list.read(cx).overrides.get(&replayed.id).is_some_and(|meta| meta.name.is_some());
        replayed.turns = view.read(cx).rt(cx).session().map(|s| s.turns.len() as u64).unwrap_or(0);
        // The replayed row stands for the folded capture, and no wire event
        // will ever sync it the way `session/list` does live: read the open
        // session's own words off the view here — the same overlay
        // `render_row_detail` applies every frame.
        {
            let view = view.read(cx);
            let (approval, question) = view.rt(cx).row_pending();
            if approval.is_some() {
                replayed.approval_command = approval;
            }
            if question.is_some() {
                replayed.pending_question = question;
            }
            if replayed.last_ask.as_deref().map(str::trim).is_none_or(|s| s.is_empty()) {
                replayed.last_ask = view.rt(cx).last_user_text();
            }
            if replayed.description.trim().is_empty() {
                if let Some(summary) = view.rt(cx).last_summary_text() {
                    replayed.description = summary;
                }
            }
        }
        self.session_list.update(cx, |list, _| list.sessions = vec![replayed]);
        // A scripted sidebar joins the replayed row, as if the wire had
        // listed it beside the capture.
        self.apply_sidebar_fixture(cx);
        // The replay's one row is the whole list, and there is no index
        // to wait for: both have landed.
        self.session_list.update(cx, |list, _| list.sessions_loaded = true);
        self.session_list.update(cx, |list, _| list.index_loaded = true);
        self.invalidate_list(cx);
        // A replayed window has no wire, but the search palette still needs
        // the host's session index: read it (read-only) and rebuild `search.db`
        // so `--steps search:<query>` screenshots show session hits.
        self.load_index(cx);
        let tier_banner = self.tier_banner();
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_tier_banner(tier_banner, cx)));
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::PageMenu, cx); });
        self.active = Some(view);
        // The replayed session is current, so its agent routes may drive
        // the terminal (D53): register it for this window's project root.
        // Live lanes register on their own open paths (T2 owns those).
        let replayed = self
            .active
            .as_ref()
            .map(|view| view.read(cx))
            .map(|view| (view.rt(cx).session_id.clone(), view.rt(cx).provider_kind()));
        if let Some((session_id, provider)) = replayed {
            let root = self
                .current_project(cx)
                .map(|project| project.root.clone())
                .unwrap_or_else(|| self.args.workspace.clone());
            self.register_terminal_session(&session_id, root, provider.as_str(), cx);
        }
        // The capture is already folded, and no wire event will ever run
        // `title_from_transcript` for it: without this the replayed row
        // keeps the file's name even when the transcript knows better.
        if let Some(view) = self.active.clone() {
            let runtime = view.read(cx).runtime().clone();
            self.title_from_transcript(&runtime, cx);
        }
        // `--session <id>`: the live boot opens it once the list arrives
        // (`load_sessions`), and a replay window has no list reply — so the
        // landed fixture is the arrival it waits for. A run with no session
        // named keeps the replayed view. Only once the replay is `active`:
        // called before, a run with `--steps` saw no session and opened an
        // empty draft over the replay, and the header named the draft ("New
        // session") above the replayed transcript.
        self.open_boot_session(window, cx);
        #[cfg(feature = "probe")]
        self.maybe_run_steps(window, cx);
        cx.notify();
    }

    /// While a switch to another session is pending (Q4): the centre is the
    /// destination's loading state — never the previous session's transcript
    /// or composer draft. Muse opens already swap synchronously with
    /// `mark_history_loading`, so this only ever names a provider reopen
    /// whose child is still resuming; a marked replace (in-place provider
    /// switch) and the inline failure keep their own views.
    pub(crate) fn pending_switch_dest(&self, cx: &gpui::App) -> Option<String> {
        if !self.open_state.switch_pending {
            return None;
        }
        if self.open_state.is_replacing() {
            return None;
        }
        if self.open_state.provider_open_error.is_some() {
            return None;
        }
        let dest = self.open_state.pending_id.clone()?;
        if self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == dest) {
            return None;
        }
        Some(dest)
    }

    /// Whether the right pane must stay neutral this frame (Q4c): while a
    /// provider switch is pending the centre is the destination's loading
    /// state, so the pane keeps its width — the centre never resizes —
    /// but shows nothing session-specific and no webview. Closing the pane
    /// instead would grow the centre for the switch's duration and snap it
    /// back on landing; the neutral body avoids that flicker. The render
    /// and the browser sync both gate on this.
    pub(crate) fn pending_right_neutral(&self, cx: &gpui::App) -> bool {
        self.pending_switch_dest(cx).is_some()
    }

    /// The centre while a provider switch is pending: the same neutral
    /// history-loading shape a Muse reopen shows (`loading_row` + the
    /// "Loading history…" status) — an empty pane that keeps its height,
    /// with no transcript and no composer, so no previous-session content
    /// can paint. No interactive controls, so no control roles are needed.
    fn render_pending_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        v_flex()
            .size_full()
            .child(div().w_full().flex_1().min_h_0())
            .child(
                div()
                    .w_full()
                    .px(px(scale::SP_4))
                    .pb(px(scale::SP_4))
                    .child(
                        div()
                            .w_full()
                            .ui(TextRole::Caption)
                            .text_color(p.fg_muted)
                            .child("Loading history…"),
                    ),
            )
            .into_any_element()
    }

    /// The Browse centre (B2a-H2): the current project's active person
    /// tab's page. The page is built here, where it shows, never as a
    /// render side effect elsewhere. With no person tab open the centre
    /// names itself and offers a New tab — render never opens one (B2a-H2b).
    fn render_browse_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Bookmarks and History fill the centre while open (docs/82 8.6);
        // the occlusion hides the native page under them.
        if self.library_open(cx) {
            if let Some(page) = self.browser.read(cx).library.clone() {
                return page.into_any_element();
            }
        }
        let harness = cx.weak_entity();
        let mut blank_page = false;
        let body = match self.browse_centre_page(window, cx) {
            Some(state) => {
                // Blank only when the TAB is blank too: during a slow first
                // load the live page still reports about:blank, which drew
                // the new-tab page over a tab already navigating (seen live
                // 2026-10-11).
                let blank = matches!(state.read(cx).url().as_ref(), "" | crate::browser::tabs::BLANK)
                    && self.browse_centre_tab_blank(cx);
                // `about:blank` tabs render the Baaz new-tab page instead of
                // a white page — gpui, since about:blank is hidden by the
                // library.
                if blank {
                    blank_page = true;
                    self.ensure_newtab_ground(cx);
                    self.render_browse_newtab(window, cx)
                } else if let Some(reason) =
                    state.read(cx).crashed().map(|crashed| crashed.to_string())
                {
                    // A crashed tab renders the Baaz error page in the page
                    // rect (B2a-H5) — gpui, while the covering rule hides
                    // the native view under it. Reload navigates again at
                    // the tab's own URL, which clears the crash.
                    self.render_browse_crashed(reason, cx)
                } else {
                    // Browse draws its own toolbar above, so the page fills
                    // the centre with no library nav row of its own.
                    // (Annotate and Screenshot stay reachable through the
                    // header's ⋯ menu.)
                    // The page fills its column edge to edge (docs/83 §3):
                    // square native corners (a tab that was the Peek's
                    // rounded page gets them back here).
                    state.update(cx, |state, _| state.set_corner_radius(0.0));
                    // The agent cursor (docs/82 8.6) over a page gpui draws;
                    // over a native page its own float draws it.
                    let cursor = (!state.read(cx).is_native()).then(|| self.agent_cursor_layer(window, cx)).flatten();
                    let page = aui_webview::webview_pane("browse-centre-page", &state)
                        .without_nav()
                        .on_intent(move |intent, window, cx| {
                            if let Some(harness) = harness.upgrade() {
                                harness.update(cx, |harness, cx| {
                                    harness.handle_browser_intent(intent, window, cx);
                                });
                            }
                        })
                        .into_any_element();
                    let page = match cursor {
                        Some(cursor) => div().relative().size_full().child(page).child(cursor).into_any_element(),
                        None => page,
                    };
                    match self.render_split(page, window, cx) {
                        Ok(split) => split,
                        Err(page) => page,
                    }
                }
            }
            None => self.render_browse_empty(cx),
        };
        // The find and permission strips are rows in the chrome above the
        // page (B2a-H5): in flow, so the page rect shrinks under them —
        // never drawn over it. The omnibox dropdown and the chrome
        // popovers drop down over the page instead — painted after it, so
        // gpui draws them above the screenshot stand-in the covering rule
        // shows while they stand open.
        let find = (!blank_page).then(|| self.render_browse_find(window, cx)).flatten();
        let permission = (!blank_page).then(|| self.render_browse_permission(cx)).flatten();
        let body = if find.is_some() || permission.is_some() {
            div().flex_1().min_h_0().w_full().child(body).into_any_element()
        } else {
            body
        };
        // With the sidebar hidden there is no overlay window for the Field:
        // its panel draws in this window over the page — painted after it,
        // so gpui draws it above the screenshot stand-in the covering rule
        // shows while it has rows. The new-tab field renders its own list
        // in flow, under the centred field; the Field overlay carries its
        // own rows over the page, so no second list draws under it.
        let omnibox = (!blank_page && !self.field_overlay_wanted(cx))
            .then(|| self.render_field_in_window(window, cx))
            .flatten();
        // The sidebar's downloads popover draws in the overlay window, over the live page.
        let downloads = (!self.browser.read(cx).downloads_overlay_wanted(cx)).then(|| self.render_browse_downloads(window, cx)).flatten();
        // The footer's extensions popover draws in the overlay window too.
        let extensions = (!self.browser.read(cx).downloads_overlay_wanted(cx)).then(|| self.render_browse_extensions(window, cx)).flatten();
        // A click outside the popovers dismisses them, like the tab menu:
        // the catcher is a sibling below the popovers in the same relative
        // column, so popover clicks still land on the popovers.
        let scrim: Option<AnyElement> =
            (downloads.is_some() || extensions.is_some()).then(|| {
                let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
                    this.dismiss_browse_popovers(cx);
                });
                dismiss_layer("browse-popover-scrim", "Dismiss menu", move |w, cx| dismiss(&(), w, cx)).into_any_element()
            });
        let peek = self.render_peek(cx);
        let peeking = peek.is_some();
        let centre = v_flex()
            .size_full()
            .relative()
            .id("browse-centre")
            // R4: Esc closes the Peek and ⌘↩ makes it a tab while gpui holds
            // the keyboard (the page's own keys come through the router).
            .when(peeking, |d| {
                d.on_key_down(cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, _, cx| {
                    let k = &event.keystroke;
                    if k.key == "escape" && !k.modifiers.platform {
                        this.browser.update(cx, |browser, cx| browser.close_peek(cx));
                        cx.stop_propagation();
                    } else if k.key == "enter" && k.modifiers.platform {
                        this.peek_to_tab(cx);
                        cx.stop_propagation();
                    }
                }))
            })
            .role(gpui::Role::Group)
            .aria_label("Browse")
            .children(find)
            .children(permission)
            .child(body)
            .children(omnibox)
            .children(scrim)
            .children(downloads)
            .children(extensions)
            .children(peek);
        // Flat (docs/83 §3): this fills the shell's centre column edge to
        // edge; the native page's rect and its square corners match
        // (`browse_centre_page_rect`, `set_corner_radius`). The loading line
        // rides the column's top edge.
        // The docked agent column (R3) takes a strip on the column's right:
        // the page narrows beside it (its own window covers the strip), and
        // `browse_centre_page_rect` agrees.
        let column = if self.browser.read(cx).agent_window.card_ui == agent_window::CardUi::Column { agent_window::COLUMN_W + 6.0 } else { 0.0 };
        div()
            .size_full()
            .relative()
            .pr(px(column))
            .child(centre)
            // The loading line is the Browse view's own (task 4.5b).
            .child(self.browser.clone())
            .into_any_element()
    }

    /// Start the new tab's ground bake once per launch (off the main
    /// thread; [`crate::newtab_ground`]). Captures under the deterministic
    /// clock skip the desktop picture and bake the mesh, so they never
    /// depend on the machine's wallpaper.
    fn ensure_newtab_ground(&mut self, cx: &mut Context<Self>) {
        if self.browser.read(cx).newtab_ground_asked {
            return;
        }
        self.browser.update(cx, |browser, _| browser.newtab_ground_asked = true);
        let source = if crate::runtime::captures() { None } else { crate::newtab_ground::current_source() };
        let bake = cx.background_executor().spawn(async move { crate::newtab_ground::bake(source) });
        cx.spawn(async move |this, cx| {
            let baked = bake.await;
            let _ = this.update(cx, |this, cx| {
                this.browser.update(cx, |browser, _| browser.newtab_ground = baked);
                cx.notify();
            });
        })
        .detach();
    }

    /// The Baaz new-tab page (canvas v27, owner 2026-10-11: board NewTab2 on
    /// "B with C's background"). The ground is the desktop picture, blurred
    /// and dimmed with B's centre scrim ([`crate::newtab_ground`]). On it,
    /// one 560 column a quarter of the way down holds:
    /// - the Field inline on FF's composer chrome (`input-message.tsx`: p-2,
    ///   container radius, subtitle 14/20; surface-3, edge = foreground 20 %
    ///   when focused, else the border; the float shadow over the picture);
    /// - "Pick up where you left off" as FF menu rows (36 tall, px-2, gap-2,
    ///   item radius; muted at rest, foreground when hovered) on a panel.
    ///
    /// Adaptation: gpui has no backdrop blur, so the panel is surface-1 at
    /// 62 % over the already-blurred ground, which reads as the mock's frost.
    /// The rows are a row list: each row hovers plainly with the library's
    /// one hover model (no gliding block, docs/83 §9).
    fn render_browse_newtab(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let editing = self.browser.read(cx).browse_address.is_some();
        let field_w = px(NEWTAB_FIELD_W);
        let lit = |on: bool| icon(IconName::Search).size(metrics.icon).color(if on { p.fg } else { p.fg_muted }).lit(on);
        let shell = |editing: bool| {
            aui::interact::Shape::Container.rounded(
                h_flex()
                    .w(field_w)
                    .h(metrics.control + metrics.item_px * 2.0)
                    .px(metrics.item_px * 2.0)
                    .gap(metrics.gap)
                    .items_center()
                    .bg(p.surface(3))
                    .border_1()
                    .border_color(if editing { p.fg.alpha(0.2) } else { p.line })
                    .shadow(p.floating_shadow())
                    // FF's composer text: subtitle 14/20 at weight 400 (our
                    // Subtitle role carries the semibold heading weight).
                    .text_role(TextRole::Subtitle)
                    .font_weight(gpui::FontWeight::NORMAL),
            )
        };
        let asks = div().flex_none().role_in(TextRole::Micro, cx).text_color(p.fg_muted).child("⌘↵ asks");
        let field: AnyElement = if editing {
            shell(true).child(lit(true)).child(div().flex_1().min_w_0().child(self.browse_address_input(window, cx))).child(asks).into_any_element()
        } else {
            let focus = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.focus_browse_address_with("", window, cx);
            });
            interactive("browse-newtab-field", shell(false).text_color(p.fg_muted).child(lit(false)).child(div().flex_1().child("Search, open a page, or ask")).child(asks))
                .shape(aui::interact::Shape::Container)
                .label("New tab. Search, open a page, or ask")
                .on_click(focus)
                .into_any_element()
        };
        let mut column = v_flex().w(field_w).mt(px(scale::SP_7 * 6.25)).gap(px(scale::SP_6)).child(field);
        // Typing drops the Field's own rows under it (the omnibox list);
        // the pick-up panel steps aside while they show.
        let dropdown = self.render_omnibox_dropdown(window, cx);
        let typing = dropdown.is_some();
        if let Some(list) = dropdown {
            column = column.child(list);
        }
        let pickup = if typing { Vec::new() } else { self.browse_pickup_full(cx) };
        if !pickup.is_empty() {
            let mut rows = v_flex()
                .id("browse-pickup-list")
                .w_full()
                .p(metrics.item_px / 2.0)
                .role(gpui::Role::List)
                .aria_label("Pick up where you left off")
                .child(
                    div()
                        .h(metrics.control_md)
                        .px(metrics.item_px)
                        .flex()
                        .items_center()
                        .role_in(TextRole::Caption, cx)
                        .text_color(p.fg_muted)
                        .child("Pick up where you left off"),
                );
            let hover = self.browser.read(cx).browse_pickup_hover;
            let report = cx.entity().downgrade();
            // Pick-up rows press on mouse-down (pointer timing: the press
            // must land before the address input's blur drops the field)
            // and also answer clicks for accessibility presses; the second
            // call of the pair is a no-op (selecting the same tab, reopening
            // an already-reopened tab, or opening the now-active session
            // again changes nothing).
            let mut targets: Vec<crate::browser::PickupTarget> = Vec::new();
            let mut list = aui::interact::row_list("browse-pickup-rows")
                .label("Pick up where you left off")
                .gap(Pixels::ZERO)
                .on_highlight(move |index, _, cx| {
                    let _ = report.update(cx, |this, cx| {
                        if this.browser.read(cx).browse_pickup_hover != index {
                            this.browser.update(cx, |browser, _| browser.browse_pickup_hover = index);
                            cx.notify();
                        }
                    });
                });
            for (index, (row, target)) in pickup.into_iter().enumerate() {
                let lit = hover == Some(index);
                // A tab row's favicon when it has one (open or recently
                // closed), else its letter tile; a session row its glyph.
                let favicon = match &target {
                    crate::browser::PickupTarget::Tab(id) => self.browser.read(cx).tabs.get(*id).and_then(|tab| tab.favicon.clone()),
                    crate::browser::PickupTarget::Closed(id) => self.browser.read(cx).tabs.closed.iter().find(|tab| tab.id == *id).and_then(|tab| tab.favicon.clone()),
                    crate::browser::PickupTarget::Session(_) => None,
                }
                .and_then(|url| self.browser.read(cx).favicons.file(&url).cloned());
                let lead: AnyElement = match (&target, favicon) {
                    (crate::browser::PickupTarget::Session(_), _) => icon(IconName::List).size(metrics.icon).color(p.fg_muted).into_any_element(),
                    (_, Some(path)) => gpui::img(path).size(metrics.icon).into_any_element(),
                    _ => aui::interact::Shape::Item
                        .rounded(
                            div()
                                .size(metrics.icon)
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(p.surface(4))
                                .role_in(TextRole::Micro, cx)
                                .semibold()
                                .text_color(p.fg_muted)
                                // The title's own letter: `tab_initial` reads an
                                // empty URL as a blank tab ("N").
                                .child(row.title.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect::<String>()).unwrap_or_else(|| "•".to_owned())),
                        )
                        .into_any_element(),
                };
                let content = aui::interact::Shape::Item.rounded(
                    h_flex()
                        .w_full()
                        .h(metrics.control)
                        .px(metrics.item_px)
                        .gap(metrics.gap)
                        .items_center()
                        .text_color(if lit { p.fg } else { p.fg_muted })
                        .child(div().flex_none().size(metrics.icon).flex().items_center().justify_center().child(lead))
                        .child(div().flex_shrink(1.0).min_w_0().truncate().text_role(TextRole::Body).child(row.title.clone()))
                        .child(div().flex_1())
                        .child(div().flex_none().pl(px(scale::SP_4)).role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(row.detail.clone())),
                );
                let (verb, key) = match &target {
                    crate::browser::PickupTarget::Tab(_) => ("Open", "browse-pickup"),
                    crate::browser::PickupTarget::Session(_) => ("Open", "browse-pickup-session"),
                    crate::browser::PickupTarget::Closed(_) => ("Reopen", "browse-pickup-closed"),
                };
                targets.push(target);
                list = list.item(index, content.id((key, index)).role(gpui::Role::Button).aria_label(format!("{verb} {}", row.title)));
            }
            let targets = std::rc::Rc::new(targets);
            let (on_press, on_click) = (targets.clone(), targets);
            let press = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
                if let Some(target) = on_press.get(*index) {
                    this.open_pickup_target(target, window, cx);
                }
            });
            let open = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
                if let Some(target) = on_click.get(*index) {
                    this.open_pickup_target(target, window, cx);
                }
            });
            let list = list.on_press(move |index, w, cx| press(&index, w, cx)).on_activate(move |index, w, cx| open(&index, w, cx));
            rows = rows.child(list);
            // The frosted panel: the ground at 62 %, a `line-strong` edge and
            // the float shadow (docs/83 §2.3). The picture under it is dimmed
            // dark in both themes, so light's white at 62 % came out mid-grey
            // under `fg-muted` text (2.4:1): light fills it opaque with the
            // Field's own `surface-3` (docs/83 §9, task 2.11).
            let panel_fill = if cx.aui().kind.is_dark() { p.bg.alpha(0.62) } else { p.surface(3) };
            let panel = aui::interact::Shape::Container.rounded(div().w_full().bg(panel_fill).border_1().border_color(p.line_strong).shadow(p.floating_shadow()).child(rows));
            column = column.child(panel);
        }
        // The new tab is the page column, square like a page (docs/83 §3).
        let ground = self.browser.read(cx).newtab_ground.clone().map(|path| {
            gpui::img(path).absolute().top_0().left_0().size_full().object_fit(gpui::ObjectFit::Cover).into_any_element()
        });
        div()
            .size_full()
            .relative()
            .id("browse-newtab")
            .role(gpui::Role::Group)
            .aria_label("New tab")
            .children(ground)
            .child(div().size_full().flex().flex_col().items_center().child(column))
            .into_any_element()
    }

    /// What a pick-up row does: select the tab, open the session (unless
    /// it is already the active one), or reopen exactly the closed tab.
    fn open_pickup_target(&mut self, target: &crate::browser::PickupTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            crate::browser::PickupTarget::Tab(id) => self.select_browse_sidebar_tab(*id, window, cx),
            crate::browser::PickupTarget::Session(session) => {
                if self.active_id(cx).as_deref() != Some(session.as_str()) {
                    self.set_mode(layout::Mode::Sessions, window, cx);
                    self.resume(session.clone(), window, cx);
                }
            }
            crate::browser::PickupTarget::Closed(id) => self.reopen_browse_closed_tab(*id, window, cx),
        }
    }

    /// The new-tab Field's suggestion list (B2a-H4): Ask the agent, Tabs and
    /// sessions, History and pages you read, then the engine row — in flow
    /// under the new-tab page's own centred field. Everywhere else the Field
    /// is the library panel ([`Self::field_panel`]). Rows are list-box
    /// options; ↑/↓/Tab/⌘↵/Enter ride the field's own keys. Rows press on
    /// mouse-down (pointer timing: before the address input's blur drops
    /// the field) and also answer clicks for accessibility presses; the
    /// second call is a no-op.
    fn render_omnibox_dropdown(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.browse_omnibox_open(cx) {
            return None;
        }
        let rows = self.browse_suggestions(cx);
        if rows.is_empty() {
            return None;
        }
        Some(self.omnibox_list(&rows, window, cx).w(px(NEWTAB_FIELD_W)).into_any_element())
    }

    /// The new-tab list box itself, unsized: section headers and rows for
    /// the already-ranked `rows` ([`Self::render_omnibox_dropdown`]).
    /// Row-list indices for the suggestion lists' group labels: past any
    /// row index, static (never lit, never activated).
    pub(crate) const OMNI_HEADER_BASE: usize = 10_000;

    fn omnibox_list(
        &self,
        rows: &[crate::browser::OmniRow],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let selected = self.browser.read(cx).browse_omnibox_selected.min(rows.len().saturating_sub(1));
        // FF's dropdown chrome (`dropdown.tsx`): the popup surface, a 4 px
        // inset, 36 px rows on the item radius lit by the hover overlay,
        // Caption group labels — the same rows the Field overlay draws.
        let list = menu_panel(gpui::Empty, cx)
            .id("browse-omni-list")
            .debug_selector(|| "browse-omni-list".into())
            .flex()
            .flex_col()
            .role(gpui::Role::ListBox)
            .aria_label("Address suggestions");
        // One highlight that the pointer and the arrows both drive — a hover
        // moves the selection, so Enter opens what is lit. The row list paints
        // only the selection (`pointer(false)`), so the two never light two
        // rows at once.
        let report = cx.entity().downgrade();
        // Rows press on mouse-down (pointer timing: before the address
        // input's blur drops the field) and also answer clicks, so an
        // accessibility press reaches them. The second call of the pair is
        // a no-op: every arm drops the field first, so the click re-ranks to
        // no rows — and the session arms guard anyway.
        let press = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            this.activate_omnibox_row(*index, window, cx);
        });
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            this.activate_omnibox_row(*index, window, cx);
        });
        let mut rows_list = aui::interact::row_list("browse-omni-rows")
            .pointer(false)
            .label("Address suggestions")
            .gap(Pixels::ZERO)
            .highlighted(Some(selected))
            .on_press(move |index, w, cx| press(&index, w, cx))
            .on_activate(move |index, w, cx| activate(&index, w, cx))
            .on_highlight(move |index, _, cx| {
                if let Some(index) = index {
                    let _ = report.update(cx, |this, cx| this.browser.update(cx, |browser, cx| browser.move_omnibox(crate::browser::OmniboxStep::Hover(index), cx)));
                }
            });
        let mut last_kind: Option<crate::browser::SuggestKind> = None;
        for (index, row) in rows.iter().enumerate() {
            let header = match row.suggestion.kind {
                crate::browser::SuggestKind::Ask => Some("Ask the agent"),
                crate::browser::SuggestKind::Tab | crate::browser::SuggestKind::Session
                    if !matches!(last_kind, Some(crate::browser::SuggestKind::Tab) | Some(crate::browser::SuggestKind::Session)) =>
                {
                    Some("Tabs and sessions")
                }
                crate::browser::SuggestKind::Command
                    if !matches!(last_kind, Some(crate::browser::SuggestKind::Command)) =>
                {
                    Some("Commands")
                }
                crate::browser::SuggestKind::Bookmark
                    if !matches!(last_kind, Some(crate::browser::SuggestKind::Bookmark)) =>
                {
                    Some("Bookmarks")
                }
                crate::browser::SuggestKind::Folder
                    if !matches!(last_kind, Some(crate::browser::SuggestKind::Folder)) =>
                {
                    Some("Folders")
                }
                crate::browser::SuggestKind::History
                    if !matches!(last_kind, Some(crate::browser::SuggestKind::History)) =>
                {
                    Some("History and pages you read")
                }
                _ => None,
            };
            if let Some(header) = header {
                rows_list = rows_list.item_static(
                    Self::OMNI_HEADER_BASE + index,
                    div().h(metrics.control_md).px(metrics.item_px).flex().items_center().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(header),
                );
            }
            last_kind = Some(row.suggestion.kind);
            let lit = index == selected;
            let mut item = h_flex()
                .w_full()
                .items_center()
                .gap(metrics.gap)
                .px(metrics.item_px)
                .h(metrics.control)
                .rounded(px(scale::SHAPE_ITEM))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_role(TextRole::Body)
                        .text_color(if lit { p.fg } else { p.fg_muted })
                        .child(row.suggestion.title.clone()),
                );
            // A history row's address, or the line its words matched (R4).
            if let Some(detail) = row.suggestion.detail.clone() {
                item = item.child(
                    div().flex_shrink(1.0).min_w_0().max_w(gpui::relative(0.5)).truncate().role_in(TextRole::Micro, cx).text_color(p.fg_muted).child(detail),
                );
            }
            if !row.suggestion.hint.is_empty() {
                item = item.child(div().flex_none().role_in(TextRole::Micro, cx).text_color(p.fg_muted).child(row.suggestion.hint.clone()));
            }
            rows_list = rows_list.item(
                index,
                item.id(("browse-omni", index)).role(gpui::Role::ListBoxOption).aria_label(row.suggestion.title.clone()),
            );
        }
        let _ = window;
        list.child(rows_list)
    }

    /// The Browse centre with no person tab (B2a-H2b): the empty state
    /// names the key that opens one and carries its own New-tab button
    /// (role and label from the library button). Render-only: the
    /// button — like entering Browse or closing the last person tab —
    /// is what opens tabs, never this.
    fn render_browse_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let new_tab = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.new_browse_sidebar_tab(window, cx);
        });
        v_flex()
            .size_full()
            .id("browse-empty")
            .items_center()
            .justify_center()
            .gap(px(scale::SP_4))
            .role(gpui::Role::Group)
            .aria_label("Browse, no tabs")
            .child(
                div()
                    .ui(TextRole::Caption)
                    .text_color(p.fg_muted)
                    .child("No tabs — ⌘T for a new tab"),
            )
            .child(button("browse-new-tab", "New tab").primary().on_click(new_tab))
            .into_any_element()
    }

    fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let banner = self.render_wire_banner(cx);
        let mut provider_banner = self.render_provider_banner(cx);
        // The transcript column has its own cached entity boundary (owner
        // round 6, part 4): a sidebar-only frame reuses the retained
        // transcript instead of rebuilding it. The composer band and the
        // drop overlay compose live beside it — the band's textarea would
        // pin any cached ancestor dirty on every paint, and a hidden
        // overlay would keep asking for the next frame while its exit runs.
        // `render_no_session` stays inline — there is no view to cache on,
        // and the screen is static.
        // Route::Skills: the page replaces the transcript area while open.
        // Q4: while a switch to another session is pending, the centre is
        // the destination's loading state — never the previous transcript.
        let pending_dest = self.pending_switch_dest(cx);
        let body = if self.skills_open(cx) {
            self.compose_skills_page(cx)
        } else if pending_dest.is_some() {
            self.render_pending_switch(cx)
        } else if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
            self.render_browse_centre(window, cx)
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
                    view.clone().cached(StyleRefinement::default().size_full()).into_any_element()
                };
                let band = view.update(cx, |view, cx| view.render_composer_band(window, cx));
                let jump = view.update(cx, |view, cx| view.render_jump_pill(cx));
                // The composer floats over the transcript (owner 2026-10-15:
                // "it's not floating on top of the transcript"): the band is
                // absolute at the column's bottom and the list runs under it.
                // Flat (docs/83 §1, §9 task 2.6): the band's ground is the
                // column's own, solid from the box's top edge down, so text
                // scrolled under the quiet row is hidden — no fade, no
                // lead-in. The band's painted height goes to the session
                // view, whose list ends that far up. The corner stack rides
                // the band: its bottom 8 above the composer.
                let ground = cx.aui().colors.surface_1;
                let inset_to = view.downgrade();
                let band = div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .bg(ground)
                    // Measured on the composer itself (and the signed-out
                    // banner riding it), so the list's end, the status stack
                    // and the caret menus clear it (owner 2026-10-15).
                    .child(
                        div()
                            .w_full()
                            .on_children_prepainted(move |bounds, _, cx| {
                                let height = bounds.first().map(|b| f32::from(b.size.height)).unwrap_or(0.0);
                                let _ = inset_to.update(cx, |view, cx| view.set_bottom_inset(height, cx));
                            })
                            // The signed-out banner rides the composer (D5
                            // B5), inside the measured child so the list
                            // ends above both.
                            .child(
                                v_flex()
                                    .w_full()
                                    .relative()
                                    .children(provider_banner.take())
                                    .child(band)
                                    // Off this column's top, so it clears the
                                    // signed-out banner as well as the stack.
                                    .children(jump),
                            ),
                    )
                    .children(self.render_corner_stack(cx).map(Self::corner_above_band))
                    .into_any_element();
                let overlay = view.update(cx, |view, cx| view.render_drop_overlay(window, cx));
                // ⌘F's bar (docs/82 7.5) above the transcript, composed live
                // like the band so typing never rebuilds the transcript; the
                // list gives up its height while it stands.
                let find = view.update(cx, |view, cx| view.render_find_bar(window, cx));
                // The four regions named once, on the first run's first
                // composer (docs/82 8.7), above everything in the column.
                let regions = self.render_regions_strip(cx);
                v_flex()
                    .size_full()
                    .relative()
                    .children(regions)
                    .children(find)
                    .child(div().w_full().flex_1().min_h(px(0.0)).child(transcript))
                    .child(band)
                    .children(overlay)
                    // gpui reports an external drag only while it moves, so
                    // that is what raises the overlay; the drop takes it
                    // down again.
                    .on_drag_move(cx.listener(
                        |this: &mut Self, _: &gpui::DragMoveEvent<ExternalPaths>, _, cx| {
                            // Z7a: an external drag covers the browser pane —
                            // the native view hides while it lasts.
                            this.browser.update(cx, |browser, _| browser.drop_cover = true);
                            this.with_session(cx, |view, cx| view.note_drag_over(cx));
                        },
                    ))
                    .on_drop(cx.listener(|this: &mut Self, paths: &ExternalPaths, _, cx| {
                        this.browser.update(cx, |browser, _| browser.drop_cover = false);
                        this.with_session(cx, |view, cx| view.drop_external(paths, cx));
                    }))
                    .into_any_element()
            }
            None => match self.open_state.provider_open_error.clone() {
                // Z4: the failed session's own place — never another
                // session's transcript behind a dialog.
                Some(failure) => self.render_provider_open_error(window, cx, &failure),
                None => self.render_no_session(window, cx),
            },
            }
        };
        // A centre with no composer (no session, Skills, a pending switch)
        // holds the corner stack 12 above the column's bottom; Browse's is
        // in the corner window.
        let in_band = !self.skills_open(cx) && pending_dest.is_none() && crate::layout::Layout::get(cx).mode != layout::Mode::Browse && self.active.is_some();
        let body = match (in_band || self.corner_in_overlay(cx)).then_some(()).map_or_else(|| self.render_corner_stack(cx), |_| None) {
            // A flex column, never a block: the Skills page's root is
            // `flex_1`, and in a block parent it took no height — a corner
            // stack put the page in this wrapper and Skills drew a blank
            // body (QA 2026-10-30).
            Some(stack) => v_flex().relative().flex_1().min_h_0().w_full().child(body).child(Self::corner_at_sheet_bottom(stack)).into_any_element(),
            None => body,
        };
        let dock = self.compose_terminal_dock(cx);
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
            .on_action(cx.listener(|this, _: &MenuLeft, _, cx| this.menu_sideways(-1, cx)))
            .on_action(cx.listener(|this, _: &MenuRight, _, cx| this.menu_sideways(1, cx)))
            .on_action(cx.listener(|this, _: &HistoryPrev, window, cx| {
                this.with_session(cx, |view, cx| view.history_prev(window, cx));
            }))
            .on_action(cx.listener(|this, _: &HistoryNext, window, cx| {
                this.with_session(cx, |view, cx| view.history_next(window, cx));
            }))
            .on_action(cx.listener(|this, _: &TogglePlan, _, cx| {
                this.with_session(cx, |view, cx| {
                    let on = !view.rt(cx).plan_mode();
                    view.runtime().clone().update(cx, |rt, cx| rt.set_plan(on, cx));
                });
            }))
            .on_action(cx.listener(|this, _: &PasteMaybeImage, window, cx| this.paste(window, cx)))
            .on_action(cx.listener(|this, _: &AttachFile, _, cx| {
                this.with_session(cx, |view, cx| view.prompt_for_image(cx));
            }))
            .on_action(cx.listener(|this, _: &ConfirmField, window, cx| {
                // ⏎ in the composer is always the composer's, even with a
                // card field open somewhere above: words typed there are a
                // message, never the field's send.
                this.with_session(cx, |view, cx| {
                    if view.composer_focused(window, cx) || !view.confirm_field(window, cx) {
                        view.send(window, cx);
                    }
                });
            }))
            .on_action(cx.listener(|this, _: &FindInTranscript, window, cx| {
                // docs/82 7.5: Sessions with a session open; Settings and
                // Skills keep their own ⌘F (their contexts sit deeper), and
                // Browse never reaches here (the binding excludes it).
                if this.transcript_find_available(cx) {
                    this.with_session(cx, |view, cx| view.open_find(window, cx));
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &FindNextInTranscript, _, cx| {
                let stepped = this.transcript_find_available(cx)
                    && this.active.clone().is_some_and(|view| view.update(cx, |view, cx| view.find_step(true, cx)));
                if !stepped {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &FindPrevInTranscript, _, cx| {
                let stepped = this.transcript_find_available(cx)
                    && this.active.clone().is_some_and(|view| view.update(cx, |view, cx| view.find_step(false, cx)));
                if !stepped {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CopySelection, window, cx| {
                this.with_session(cx, |view, cx| view.copy_selected(window, cx));
            }))
            .on_action(cx.listener(|this, _: &JumpNextWaiting, window, cx| {
                this.jump_to_next_waiting(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                this.toggle_terminal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPane, window, cx| {
                this.toggle_right(cx);
                // B7fix2: the key builds the real webview itself, with the
                // window — render never constructs first.
                this.prewarm_browser_in(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowRightChanges, _, cx| {
                this.select_right(layout::RightKind::Changes, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowRightFiles, _, cx| {
                this.select_right(layout::RightKind::Files, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowRightBrowser, window, cx| {
                this.select_right(layout::RightKind::Browser, cx);
                // B7fix2: the key builds the real webview itself, with the
                // window — render never constructs first.
                this.prewarm_browser_in(window, cx);
            }))
            // 1–9 on a pending approval: the n-th server-minted choice, in the
            // order the server sent them.
            .on_action(cx.listener(|this, _: &aui::keys::Cancel, window, cx| {
                // ⎋ denies a pending approval while the composer is empty;
                // otherwise the key belongs to whoever is above us — an open
                // menu closing, the palette, settings. Swallowing it here
                // broke Esc on every caret menu (owner 2026-10-15).
                let menu_open = this.overlays.read(cx).menu().is_some();
                let denied = !menu_open
                    && this.active.clone().is_some_and(|view| view.update(cx, |view, cx| view.card_has_keys(cx) && view.deny_pending(window, cx)));
                if !denied {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, nth: &aui::keys::ChooseNth, window, cx| {
                let index = nth.index;
                this.with_session(cx, |view, cx| view.choose_nth(index, window, cx));
            }))
            .children(banner)
            .child(body)
            .child(dock)
            .into_any_element()
    }

    /// The composer holder's key context for this frame.
    ///
    /// `menu`, `histup` and `histdown` are what let ↑, ↓ and ↩ mean the menu,
    /// the history or the editor without any of the three being guessed at.
    fn composer_context(&self, cx: &gpui::App) -> String {
        let mut context = String::from(COMPOSER_CONTEXT);
        if self.overlays.read(cx).menu().is_some() {
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

    /// Run `f` on the open session — unless a provider switch is pending
    /// (Q4b): the centre shows the destination's loading state while
    /// `active` is still the old view, so every harness action routed here
    /// (send, steer, stop-adjacent history/picker/approval keys) targets
    /// nothing instead of acting on the hidden session. Session verbs
    /// already wait out the switch through `steps_ready`; Muse sessions
    /// swap synchronously and never trip this.
    /// Whether ⌘F / ⌘G belong to the transcript's find (docs/82 7.5): the
    /// Sessions view with a session open and neither Settings nor Skills
    /// over it, and no switch pending.
    pub(crate) fn transcript_find_available(&self, cx: &gpui::App) -> bool {
        crate::layout::Layout::get(cx).mode == layout::Mode::Sessions
            && self.active.is_some()
            && !self.settings_open(cx)
            && !self.skills_open(cx)
            && self.pending_switch_dest(cx).is_none()
    }

    pub(crate) fn with_session(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut SessionView, &mut Context<SessionView>)) {
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| f(view, cx));
        }
    }

    /// ⌘V. An image on the clipboard becomes an attachment; anything else is
    /// the textarea's own paste, which is re-dispatched rather than reimplemented.
    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Q4b: like `with_session` — no paste into the hidden session
        // while its replacement loads, and no re-dispatch either: no
        // composer is mounted to take it.
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
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
    /// Start Muse when it is wanted and not running: a launch that left it
    /// off (not installed then, or switched off) brings it up the first time
    /// the person reaches for it, instead of handing them a dead draft.
    pub(crate) fn ensure_muse_started(&mut self, cx: &mut Context<Self>) {
        if self.providers.shared_ready() || self.args.offline || self.args.replay.is_some() {
            return;
        }
        if matches!(self.wire, Wire::Connecting | Wire::Reconnecting) {
            return;
        }
        if !crate::providers::Registry::shared_id().is_some_and(crate::provider_status::provider_enabled) {
            return;
        }
        self.wire = Wire::Connecting;
        self.connect(cx);
    }

    /// Start `id`'s shared child when its sessions ride one
    /// ([`provider::ProviderTrait::SharedChild`]); a no-op for a provider that
    /// spawns a child per session. Every open and reopen calls it before
    /// asking the factory for a lane, which waits while the child starts.
    pub(crate) fn ensure_provider_started(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        if id.has(provider::ProviderTrait::SharedChild) {
            self.ensure_muse_started(cx);
        }
    }

    /// The switcher's pick as a registry entry. `None` only for a value no
    /// registered provider answers to — which boot refuses (`--provider`)
    /// and nothing else writes — so a caller refuses rather than guessing.
    pub(crate) fn new_provider_id(&self, cx: &App) -> Option<ProviderId> {
        self.provider_model.read(cx).new_provider_id()
    }

    /// The switcher's pick as its key (see [`Self::new_provider_id`]).
    pub(crate) fn new_provider<'a>(&self, cx: &'a App) -> &'a str {
        self.provider_model.read(cx).new_provider()
    }

    pub(crate) fn select_new_provider(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        // A disabled provider is never the default for new sessions: fall
        // back to the first enabled one (the one order's first when none is).
        let id = if crate::settings_providers::live_visible_provider_ids().contains(&id) {
            id
        } else {
            crate::settings_providers::live_default_new_provider()
        };
        self.provider_model.update(cx, |model, _| model.choose_new_provider(id));
        self.ensure_provider_started(id, cx);
        cx.notify();
    }

    /// One toast per store problem ([`crate::store::take_problems`]); a
    /// problem whose toast is already up is not shown twice, so a store that
    /// keeps failing to write raises one note, not one per edit.
    pub(crate) fn raise_store_problems(&mut self, cx: &mut Context<Self>) {
        use crate::store::StoreProblem;
        let problems = crate::store::take_problems();
        if problems.is_empty() {
            return;
        }
        self.overlays.update(cx, |overlays, cx| {
            for problem in problems {
                let aside_name = |aside: &std::path::Path| {
                    aside.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
                };
                let (title, body) = match problem {
                    StoreProblem::SetAside { name, aside, restored: true } => (
                        format!("Restored {name} from its backup"),
                        format!("The file could not be read. It was kept as {}.", aside_name(&aside)),
                    ),
                    StoreProblem::SetAside { name, aside, restored: false } => (
                        format!("{name} could not be read"),
                        format!("Baaz started without it. The file was kept as {}.", aside_name(&aside)),
                    ),
                    StoreProblem::Unreadable { name, restored: true } => (
                        format!("Restored {name} from its backup"),
                        "The file could not be read, so its backup was used. The file is unchanged.".to_owned(),
                    ),
                    StoreProblem::Unreadable { name, restored: false } => (
                        format!("{name} could not be read"),
                        "Baaz started without it. The file is unchanged.".to_owned(),
                    ),
                    StoreProblem::EntriesSkipped { name, count } => (
                        format!("Part of {name} could not be read"),
                        format!(
                            "{count} {} skipped. The file is unchanged.",
                            if count == 1 { "entry was" } else { "entries were" },
                        ),
                    ),
                    StoreProblem::DatabaseUnavailable { error } => (
                        "Couldn't open Baaz's database".to_owned(),
                        format!("{error}. Changes made now will not be saved."),
                    ),
                    StoreProblem::WriteFailed { name, error } => (
                        format!("Couldn't save {name}"),
                        format!("{error}. Recent changes are not on disk yet."),
                    ),
                };
                if overlays.toasts.iter().any(|toast| toast.title.as_ref() == title) {
                    continue;
                }
                overlays.toast(title, body, cx);
            }
        });
    }

    /// Hang up every provider lane's child this window holds — the open
    /// session's, every parked one's (with a view or without) and the
    /// browser chat's (docs/81 A F6): quit shuts runtimes down, not views
    /// (docs/82 4.8), then the shared `muse serve` child, which no lane owns
    /// (docs/81 B PM-12). What app quit calls so no `claude`, `codex` or
    /// `muse` child outlives the app; dropping the runtimes would do the same through
    /// [`Drop`](crate::session::SessionRuntime), but quit should not rely on
    /// teardown order. Quitting with a switch pending cancels it too, so an
    /// open that lands after teardown is discarded instead of activating.
    ///
    /// A provider draft still opening has no child in any view yet: its
    /// open (`provider_open_work`) is told to hang the child up as soon as
    /// it answers, on the background thread, so no window is needed.
    ///
    /// Last, every command an agent started in Baaz's terminal that still
    /// runs has its process group ended (docs/82 6.5): the closing ptys hang
    /// their shells up, but a server that ignores the hang-up would outlive
    /// the app.
    pub(crate) fn shutdown_provider_views(&mut self, cx: &mut Context<Self>) {
        self.open_state.provider_opens_cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        self.cancel_pending_switch(cx);
        self.registry.shutdown_all(self.active.as_ref(), cx);
        if let Some(shared) = self.providers.shared() {
            shared.shutdown();
        }
        self.terminal_dock.read(cx).host().read(cx).end_all_tasks(cx);
    }

    /// The inline state for a provider that could not start (Z4, typed in
    /// docs/71 §3) of a session that never opened: the title, one sentence
    /// from the failure's kind, the failure's own words only when they add
    /// to it ([`crate::provider_status::failure_detail`]), the child's last
    /// stderr lines in a mono well, then
    /// Retry, the action the kind implies (Install / Sign in / Update, each
    /// the D5 flow of that name) and Providers…, over a disabled composer
    /// naming the provider. This is deliberately not a session view, so no
    /// transcript — and no tier banner — from anywhere else can render
    /// here.
    fn render_provider_open_error(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
        failure: &LaunchFailure,
    ) -> AnyElement {
        let p = cx.aui().colors;
        let id = failure.provider;
        let label = id.label();
        // The click handlers re-run the failed open, run the kind's own
        // D5 flow, or open Settings on the Providers section. Each wrapper
        // carries the role and the human label: `button` has no aria
        // builder of its own, so the header toggle wraps it the same way.
        let retry = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.retry_provider_open(window, cx);
        });
        let providers = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_providers(cx);
        });
        let (title, retry_label) = if failure.session_id.is_some() {
            (format!("Couldn't reopen {label}"), format!("Retry reopening {label}"))
        } else {
            (format!("Couldn't start {label}"), format!("Retry starting {label}"))
        };
        let sentence = failure.kind.sentence(label);
        // The raw failure only when it adds to the sentence and the well
        // (owner 2026-10-30: never the same fact twice).
        let detail = failure.kind.detail(&failure.message, &sentence, &failure.stderr_excerpt);
        let mut actions = h_flex().gap(px(scale::SP_2)).mt(px(scale::SP_2)).child(
            div()
                .id("reopen-failed-retry")
                .role(gpui::Role::Button)
                .aria_label(retry_label)
                .child(button("reopen-retry", "Retry").primary().on_click(retry)),
        );
        if let Some(action) = failure.kind.action() {
            let run = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.run_failure_action(id, action, window, cx);
            });
            actions = actions.child(
                div()
                    .id("launch-failed-action")
                    .role(gpui::Role::Button)
                    .aria_label(format!("{} {label}", action.label()))
                    .child(button("launch-failed-kind", action.label()).on_click(run)),
            );
        }
        actions = actions.child(
            div()
                .id("reopen-failed-providers")
                .role(gpui::Role::Button)
                .aria_label("Open Providers settings")
                .child(button("reopen-providers", "Providers…").on_click(providers)),
        );
        let mut body = v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(scale::SP_3))
            .pb(px(HERO_LIFT))
            .px(px(scale::SP_7))
            // The empty-state type (docs/83 §7): title Subtitle, the
            // sentence Body `fg-muted`, any detail Caption.
            .child(div().text_role(aui_tokens::TextRole::Subtitle).text_color(p.ink).child(title))
            .child(div().text_role(aui_tokens::TextRole::Body).text_color(p.fg_muted).child(sentence))
            .children(detail.map(|detail| div().text_role(aui_tokens::TextRole::Caption).text_color(p.fg_muted).child(detail)));
        if !failure.stderr_excerpt.is_empty() {
            // The child's own last words, verbatim and at most twelve
            // lines: what a person (or a bug report) needs to tell a
            // missing login from a crash.
            body = body.child(
                div()
                    .id("launch-failed-stderr")
                    .role(gpui::Role::Log)
                    .aria_label(format!("{label}'s last output"))
                    .w_full()
                    .max_w(px(LAUNCH_STDERR_MAX_W))
                    .mt(px(scale::SP_2))
                    .px(px(scale::SP_3))
                    .py(px(scale::SP_2))
                    // A nested block: one fill step up, no edge (docs/83
                    // §1 rule 3), as the dialog's detail block (2.8).
                    .rounded(px(scale::R_SM))
                    .bg(p.surface_3)
                    .mono(TextRole::Caption)
                    .text_color(p.fg_muted)
                    .child(failure.stderr_excerpt.join("\n")),
            );
        }
        // No composer stand-in under the view: "<Provider> is not available"
        // was wrong for a missing history or a dead first start, and the
        // view above already says what happened and what to do.
        v_flex().size_full().child(body.child(actions)).into_any_element()
    }

    /// The failure view's typed action (docs/71 §3), each the D5 flow of
    /// the same name: Install and Update open the setup sheet with the
    /// command typed, Sign in runs the provider's own sign-in.
    pub(crate) fn run_failure_action(
        &mut self,
        id: ProviderId,
        action: crate::provider_status::FailureAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::provider_status::FailureAction;
        match action {
            FailureAction::SignIn => self.banner_sign_in(id, window, cx),
            FailureAction::Install => {
                self.connect_provider_action(id, aui::screens::ProviderAction::Install, window, cx)
            }
            FailureAction::Update => {
                self.connect_provider_action(id, aui::screens::ProviderAction::Update, window, cx)
            }
        }
    }

    fn render_no_session(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        if self.current_project(cx).is_none() {
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
                .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink).child("Ready when you are"))
                .child(
                    div()
                        .text_role(aui_tokens::TextRole::Body)
                        .text_color(p.fg_muted)
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
        // An empty state (docs/83 §7): the mascot at 72, the title in
        // Subtitle, one Body line in `fg-muted`, one button.
        let new = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.new_session(window, cx));
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(scale::SP_3))
            .pb(px(HERO_LIFT))
            .child(crate::mascot::empty_mascot(window, cx))
            .child(div().text_role(aui_tokens::TextRole::Subtitle).text_color(p.ink).child(self.workspace_name(cx)))
            .child(
                div()
                    .text_role(aui_tokens::TextRole::Body)
                    .text_color(p.fg_muted)
                    .child("Pick a session on the left, or \u{2318}N to start one."),
            )
            .child(
                div()
                    .mt(px(scale::SP_2))
                    .child(button("new-session", "New session").primary().icon(IconName::Plus).on_click(new)),
            )
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
            self.session_list.read(cx).sessions.iter().find(|e| e.id == session_id).and_then(|e| e.workspace.clone());
        let full = match workspace {
            Some(root) => std::path::PathBuf::from(root).join(path),
            None => self.args.workspace.join(path),
        };
        if !full.is_file() {
            self.overlays.update(cx, |overlays, cx| {
                overlays.toast("File not found", format!("{path} is no longer in this workspace."), cx);
            });
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
        let scoped = !crate::layout::Layout::get(cx).search_all_projects;
        let project = self.current_project(cx).map(|p| p.name.clone()).unwrap_or_default();
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

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The whole-frame instrument's start; the trailing marker below
        // closes it after paint (see `session::draw_end_marker`).
        session::note_draw_start();
        crate::log::crumb("render");
        crate::sidebar_view::note_baaz_render();
        crate::log::note_root_render();
        self.trace_root_frame(cx);
        // The connect screen owns the whole window on a first run; the shell
        // is not built behind it. Any other launch renders the shell at once:
        // a slow or signed-out Muse never gates the window.
        let body: AnyElement = if self.connect_open(cx) {
            aui::shell::place_native_lights(window, cx);
            self.compose_connect(cx)
        } else {
            // The column is its own cached view: clean,
            // gpui reuses its retained subtree and only the centre rebuilds.
            // `size_full` is what the column wears itself (the pane's render
            // fills its cell), so the cached layout resolves to the same
            // bounds the shell offers and any resize re-renders through the
            // bounds key.
            // No `.cached(...)` while assistive tech is on — see `render_centre`.
            // B11: while Settings is open the left column shows the
            // section list instead of the session sidebar, and the
            // centre+right merge into the settings surface (right pane
            // and terminal hidden, not closed — their state is untouched
            // and restored on exit).
            let settings_open = self.settings_open(cx);
            let right_open = crate::layout::Layout::get(cx).right_open && !settings_open;
            // The Settings page fills three slots (task 4.4): its section
            // list, its surface, and the bar's crumbs and close.
            let (settings_nav, settings_surface, settings_chrome) = match settings_open.then(|| self.compose_settings(window, cx)) {
                Some((nav, surface, chrome)) => (Some(nav), Some(surface), Some(chrome)),
                None => (None, None, None),
            };
            // The column's facts go in before it is composed: only what the
            // root alone knows, and a notify of the pane only when it moved
            // (`compose_sidebar`). A root render that changed none of them
            // leaves the cached column alone.
            if settings_nav.is_none() {
                self.compose_sidebar(window, cx);
            }
            let sidebar: AnyElement = if let Some(nav) = settings_nav {
                nav
            } else if window.is_a11y_active() {
                self.sidebar_pane.clone().into_any_element()
            } else {
                self.sidebar_pane.clone().cached(StyleRefinement::default().size_full()).into_any_element()
            };
            let centre = if let Some(content) = settings_surface {
                match self.render_corner_stack(cx) {
                    Some(stack) => div().relative().size_full().child(content).child(Self::corner_at_sheet_bottom(stack)).into_any_element(),
                    None => content,
                }
            } else {
                self.render_centre(window, cx)
            };
            // The right pane is its own entity (task 4.3): this frame's facts
            // go in (the project, the active session, the pending switch, the
            // session's page — built here, where the window is at hand, while
            // the pane stands open on it), and the view comes back. It draws
            // from its own read cache — no subprocess or filesystem I/O of its
            // own — and is deliberately not `.cached(...)` like the sidebar
            // and transcript columns: the facts arrive during this render, so
            // a retained subtree would draw last frame's. Its tab row goes in
            // the column's 44 px header (docs/83 §6).
            let right = self.compose_right_pane(window, cx);
            let header_right_el = self.right_pane.update(cx, |pane, cx| pane.actions(window, cx));
            // One shell for every page (docs/83 §3, flat): the sidebar column
            // full height with the chrome row at its top; the centre and the
            // right pane plain columns, each with its 44 px header and line;
            // closed, nothing stays (not even the lights) and the top or
            // left edge peeks the sidebar. Settings puts its section list in the column,
            // its breadcrumb and close button in the centre header. Sessions animates (ease-out, the centre's content laid out
            // once at the resting width); Browse is instant — the native page
            // cannot follow a per-frame resize without jank — and cuts a hole
            // for the peek so it shows over the page.
            let browse = crate::layout::Layout::get(cx).mode == layout::Mode::Browse && !settings_open;
            let chrome = if let Some(chrome) = settings_chrome {
                self.browser.update(cx, |browser, cx| browser.cut_holes(Vec::new(), cx));
                chrome
            } else {
                self.render_bar_chrome(window, cx)
            };
            let holes = cx.entity().downgrade();
            let (right_width, right_resizing) = (self.right_width(cx), self.right_pane.read(cx).resize.active);
            let (sidebar_width, sidebar_resizing) = (self.sidebar_width(cx), self.sidebar_resizing(cx));
            let right_snap = self.right_pane.update(cx, |pane, _| pane.take_snap());
            // Painted lights off: the window owns real, glossy ones, and
            // the painted set only ever stacked underneath them.
            let mut shell = app_shell("shell")
                .native_lights(true)
                .animate(!browse)
                .page_sheet(browse)
                .sidebar_width(px(sidebar_width))
                .right_width(px(right_width))
                .resizing(sidebar_resizing || right_resizing || right_snap)
                .sidebar_open(crate::layout::Layout::get(cx).sidebar_open)
                .right_open(right_open)
                .sidebar(sidebar)
                .chrome_lead(chrome.lead)
                .peek_open(self.capture.peek_pinned())
                .on_hover_zones({
                    let zones_to = cx.entity().downgrade();
                    move |zones, _, cx| {
                        let _ = zones_to.update(cx, |this, cx| this.set_browse_hover_zones(zones, cx));
                    }
                })
                .on_overlay_bounds(move |rects, window, cx| {
                    let _ = holes.update(cx, |this, cx| {
                        // The lights belong to the sidebar's row: gone with
                        // the column, back while the peek card is on screen.
                        let hidden = !crate::layout::Layout::get(cx).sidebar_open && rects.is_empty();
                        if hidden != this.lights_hidden {
                            this.lights_hidden = hidden;
                            crate::native_lights::set_traffic_lights_hidden(window, hidden, cx);
                        }
                        this.browser.update(cx, |browser, cx| browser.cut_holes(rects, cx));
                    });
                })
                .right(right)
                .centre(centre);
            if let Some(header_right) = header_right_el {
                shell = shell.header_right(header_right);
            }
            if let Some(switch) = chrome.switch {
                shell = shell.chrome_switch(switch);
            }
            if let Some(title) = chrome.title {
                shell = shell.chrome_title(title);
            }
            if let Some(right) = chrome.right {
                shell = shell.chrome_right(right);
            }
            if !settings_open {
                shell = shell.peek(self.sidebar_pane.clone().into_any_element());
            }
            let shell = shell.into_any_element();
            // The strip sits over the divider, centred on the settled edge:
            // the handle reports the drag, this only positions it. Hidden
            // with the sidebar: there is no divider to grab on the rail.
            let mut stack = div().relative().size_full().child(shell);
            if crate::layout::Layout::get(cx).sidebar_open {
                let press = cx.entity().downgrade();
                let travel = cx.entity().downgrade();
                let release = cx.entity().downgrade();
                stack = stack.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(self.sidebar_width(cx) - RESIZE_HANDLE_W / 2.0))
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
            // the pane: there is no divider to grab while it stands closed
            // (or while Settings hides the pane without closing it).
            if right_open {
                let press = cx.entity().downgrade();
                let travel = cx.entity().downgrade();
                let release = cx.entity().downgrade();
                stack = stack.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        // Flat (docs/83 §3): the pane is a column flush with
                        // the window's right edge; the handle straddles its
                        // left divider (live 2026-10-19: a handle 9 px inside
                        // the pane was where nobody aims).
                        .right(px(right_width - RESIZE_HANDLE_W / 2.0))
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
        };
        // In Browse the overlay window hosts the modals over the live page
        // (`OverlayKind::Modal`): the main window draws neither.
        let modal_hosted = self.modal_overlay_wanted(cx);
        let dialog = if modal_hosted { None } else { self.compose_dialog(cx) };
        // The connect screen draws its own setup and import sheets
        // (`compose_connect`); drawing them here as well stacked a second
        // scrim under the first (the window behind went to 25 %).
        let connect_owns_sheets = self.connect_open(cx);
        let muse_sheet = self
            .render_muse_sheet(cx)
            .or_else(|| if connect_owns_sheets { None } else { self.compose_setup_sheet(cx) })
            .or_else(|| if connect_owns_sheets { None } else { self.render_import_sheet(cx) })
            .or_else(|| self.compose_chrome_import(cx));
        let palette = if modal_hosted { None } else { self.compose_palette(cx).or_else(|| self.compose_attach_picker(cx)) };
        // Mid-drag the overlay covers the window, so the drag survives the
        // pointer outrunning the 6 px strip; moves alone would go silent.
        // One overlay, not two: the two drags never run together, so it
        // routes every move to whichever one is in flight.
        let capture: Option<AnyElement> =
            (self.sidebar_resizing(cx) || self.right_pane.read(cx).resize.active).then(|| {
                let travel = cx.entity().downgrade();
                let release = cx.entity().downgrade();
                drag_capture_overlay("resize-capture")
                    .on_drag(move |x, _, cx| {
                        travel
                            .update(cx, |this, cx| {
                                if this.sidebar_resizing(cx) {
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
                                if this.sidebar_resizing(cx) {
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
        let right_views = self.render_right_views_menu(cx);
        let row_detail = self.render_row_detail(cx);
        let project_menu = self.render_project_menu(cx);
        let files_menu = self.right_pane.update(cx, |pane, cx| pane.render_files_menu(cx));
        // Z7a, the native-overlay rule: hide every native webview this frame
        // covers (the page is composited above gpui, so gpui cannot draw
        // over it — the host takes the view out instead), through the one
        // occlusion owner. The pages' own reconcile (navigations, history,
        // screenshots, the tabs write) runs from their changes, not here.
        self.occlude_pages(window, cx);
        // A cover waits for the covered page's picture: until it stands in,
        // the live page is still above gpui, so the cover would show only
        // where it misses the page (the scrim dimmed the sidebar first).
        // Held covers stay mounted — the palette keeps the keyboard — and
        // paint transparent; the frames keep coming until the page settles.
        let hold_covers = self.browser.read(cx).browser_cover_settling(cx);
        if hold_covers {
            aui_motion::request_frame(window);
        }
        let held = |cover: Option<AnyElement>| {
            cover.map(|cover| {
                if hold_covers {
                    div().absolute().inset_0().opacity(0.0).child(cover).into_any_element()
                } else {
                    cover
                }
            })
        };
        let (palette, dialog, muse_sheet) = (held(palette), held(dialog), held(muse_sheet));
        let (overflow, project_menu, right_views) = (held(overflow), held(project_menu), held(right_views));
        // The keyboard moves off the frame (task 4.9): the palette takes it
        // as it opens and a closed overlay gives it back from the settle
        // (`Harness::settle`), or at the end of the frame that unmounted the
        // focused element (`on_focus_lost`). The frame only notes what holds
        // it — a read.
        // What the next overlay to open gives the keyboard back to.
        let focused = window.focused(cx);
        self.overlays.update(cx, |overlays, _| overlays.stack.note_focus(focused));
        let root = aui::keys::track_pointer(
            div()
                .size_full()
                .relative()
                // Browse adds its context on the SAME node as `AuiRoot`, so
                // both sit at one depth and the later table row wins (⌘W =
                // close tab after ⌘W = close window). An ancestor wrapper
                // lost to the deeper `AuiRoot` binding; a child wrapper was
                // never on the focused path.
                .key_context(if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                    BROWSE_ROOT_CONTEXT
                } else {
                    aui::keys::ROOT_CONTEXT
                })
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
                .on_action(cx.listener(|this, _: &ToggleAgentLine, window, cx| this.toggle_agent_line(window, cx)))
                .on_action(cx.listener(|this, _: &ToggleAgentCard, _, cx| this.toggle_agent_card(cx)))
                .on_action(cx.listener(|this, _: &ToggleAgentColumn, _, cx| this.toggle_agent_column(cx)))
                .on_action(cx.listener(|this, _: &OpenModeMenu, _, cx| this.open_picker(MenuKind::Mode, cx)))
                .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
                .on_action(cx.listener(|this, _: &NewSession, window, cx| {
                    // ⌘T lands on the new-tab page with the field focused
                    // (B2a-H4): in Browse it opens a blank person tab,
                    // never a chat session.
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.new_browse_sidebar_tab(window, cx);
                        return;
                    }
                    // ⌘N leaves Settings only when the new session actually
                    // lands: `activate` exits for the navigation, so a ⌘N
                    // that starts nothing keeps the page.
                    this.new_session(window, cx)
                }))
                .on_action(cx.listener(|this, _: &AddProject, window, cx| this.open_projects(false, window, cx)))
                .on_action(cx.listener(|this, _: &Interrupt, _, cx| this.interrupt(cx)))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.open_search(window, cx)))
                .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
                .on_action(|_: &ZoomWindow, window, _| window.zoom_window())
                .on_action(cx.listener(|this, _: &ToggleTheme, window, cx| {
                    // The opposite of what the window paints, stored as the
                    // Appearance choice so Settings shows what is painted.
                    let next = if cx.aui().kind.is_dark() {
                        crate::layout::ThemePref::Light
                    } else {
                        crate::layout::ThemePref::Dark
                    };
                    this.set_theme_pref(next, Some(window), cx)
                }))
                .on_action(cx.listener(|this, _: &ShowAbout, _, cx| this.show_about(cx)))
                // ⌘, (and File → Settings…): the last-visited section this
                // run, General on first open — an empty target spells that
                // (`normalize_settings_target`). Never a toggle.
                .on_action(cx.listener(|this, _: &OpenSettings, _, cx| this.open_settings_page("", cx)))
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
                .on_action(cx.listener(|this, _: &aui::keys::ToggleRightPane, window, cx| {
                    this.toggle_right(cx);
                    // B7fix2: the key builds the real webview itself, with
                    // the window — render never constructs first.
                    this.prewarm_browser_in(window, cx);
                }))
                // ⌥⌘B anywhere in the window, including while a native
                // page holds the keyboard (the keymap claims it at the
                // CEF router, and this root binding carries it there).
                .on_action(cx.listener(|this, _: &ToggleBrowseMode, window, cx| {
                    this.toggle_browse_mode(window, cx);
                }))
                .on_action(cx.listener(|this, _: &NewBrowserTab, window, cx| {
                    this.new_browser_tab(window, cx);
                }))
                // B2a-H3: the Browse tab shortcuts. They bind in
                // `BaazBrowse` (the wrapper above), so with a page
                // focused the claimed set carries them here; each is a
                // no-op outside Browse (the context is what scopes them,
                // the guard is the belt).
                .on_action(cx.listener(|this, _: &CloseBrowseTab, window, cx| {
                    this.close_browse_shortcut(window, cx);
                }))
                .on_action(cx.listener(|this, _: &ReopenBrowseTab, window, cx| {
                    this.reopen_browse_shortcut(window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab1, window, cx| {
                    this.select_browse_nth(1, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab2, window, cx| {
                    this.select_browse_nth(2, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab3, window, cx| {
                    this.select_browse_nth(3, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab4, window, cx| {
                    this.select_browse_nth(4, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab5, window, cx| {
                    this.select_browse_nth(5, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab6, window, cx| {
                    this.select_browse_nth(6, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab7, window, cx| {
                    this.select_browse_nth(7, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTab8, window, cx| {
                    this.select_browse_nth(8, false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &BrowseTabLast, window, cx| {
                    this.select_browse_nth(9, true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &MoveBrowseTabDown, window, cx| {
                    this.move_browse_shortcut(1, window, cx);
                }))
                .on_action(cx.listener(|this, _: &MoveBrowseTabUp, window, cx| {
                    this.move_browse_shortcut(-1, window, cx);
                }))
                // B2a-H5: find, copy-link and devtools. They bind in
                // `BaazBrowse`, so with a page focused the claimed set
                // carries them here; each is a no-op outside Browse or
                // with no shown page (the context is what scopes them,
                // the guards are the belt).
                // R4: Esc / ⌘↩ for an open Peek; passed on when there is none.
                .on_action(cx.listener(|this, _: &ClosePeek, _, cx| {
                    if this.browser.read(cx).peek.is_some() {
                        this.browser.update(cx, |browser, cx| browser.close_peek(cx));
                    } else {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &PeekToTab, _, cx| {
                    if this.browser.read(cx).peek.is_some() {
                        this.peek_to_tab(cx);
                    } else {
                        cx.propagate();
                    }
                }))
                .on_action(cx.listener(|this, _: &BookmarkPage, window, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.bookmark_shown_page(window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.zoom_shown_page(1, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.zoom_shown_page(-1, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &ZoomReset, _, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.zoom_shown_page(0, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &ShowHistory, window, cx| {
                    this.toggle_library(crate::browser::library::Shelf::History, window, cx);
                }))
                .on_action(cx.listener(|this, _: &ShowBookmarks, window, cx| {
                    this.toggle_library(crate::browser::library::Shelf::Bookmarks, window, cx);
                }))
                .on_action(cx.listener(|this, _: &FindInPage, window, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.open_browse_find(window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &CopyLink, window, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse {
                        this.copy_shown_browse_link(cx);
                    }
                    let _ = window;
                }))
                .on_action(cx.listener(|this, _: &DevTools, window, cx| {
                    if crate::layout::Layout::get(cx).mode == layout::Mode::Browse
                        && this
                            .browse_centre_state(cx)
                            .is_some_and(|state| state.read(cx).supports(aui_webview::BackendFeature::DevTools))
                    {
                        this.open_browse_devtools(cx);
                    }
                    let _ = window;
                }))
                // ⌘L at window level: the webview's own `cmd-l` binding
                // only fires while gpui holds the keyboard, so this root
                // binding carries it when the native page has focus. The
                // handler itself routes to the browser only while the
                // right pane shows it.
                .on_action(cx.listener(|this, _: &aui_webview::FocusAddress, window, cx| {
                    // Ignored while the Settings page stands open (B11b): a
                    // pane-kind command, like the toggles.
                    if !this.settings_open(cx) {
                        this.focus_browser_address(window, cx);
                    }
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
                .children(palette)
                .children(dialog)
                .children(muse_sheet)
                .children(row_detail)
                .children(overflow)
                .children(right_views)
                .children(project_menu)
                .children(files_menu)
                // Painted last: the whole-frame instrument's end. Zero-size,
                // paints nothing, takes no space — captures are unaffected.
                .child(session::draw_end_marker()),
        );
        // B2a-H3b: while in Browse the window wears `BaazBrowse` on an
        // ancestor of the focus-root div below (which owns `track_focus`
        // and `AuiRoot`), never in Sessions. Focus resting on the root —
        // the first frame, after overlays close, after a routed page key —
        // still carries both contexts, so ⌘W closes a tab in Browse and
        // the window in Sessions; a child wrapper would sit below the
        // focused node and never apply.
        root.into_any_element()
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
                if crate::system_open::open_detached([dir]).is_err() {
                    crate::baaz_log!("could not reveal {}", dir.display());
                }
            }
            None => crate::baaz_log!("no docs folder beside the app"),
        }
    }

    /// ⌘⇧M / ⌃⌘E / ⌘⇧P: the same toggle the chip's own click does.
    fn open_picker(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        self.with_session(cx, |view, cx| view.toggle_picker(kind, cx));
    }

    /// Escape's first act (plan 4.6): close the newest overlay on the stack,
    /// tell the view that draws it, and run its own close. The stack queues
    /// the keyboard's way back for the root's settle. `false` when nothing was
    /// open.
    pub(crate) fn pop_overlay(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.pop_overlay_entry(cx)
    }

    /// Open `entry`, drawn by `view`, on the overlay stack: the view's own
    /// [`gpui::DismissEvent`] is how it closes (its Escape, its ✕, a click
    /// away, a finished flow), and the stack honours it newest-first
    /// ([`Self::dismiss_overlay_view`]).
    pub(crate) fn push_overlay_view<V: gpui::ManagedView>(
        &mut self,
        entry: crate::overlay_stack::Entry,
        view: &Entity<V>,
        cx: &mut Context<Self>,
    ) {
        let dismissed = cx.subscribe(view, |this: &mut Self, view, _: &gpui::DismissEvent, cx| {
            this.dismiss_overlay_view(view.entity_id(), cx);
        });
        let entry = entry.view(view.clone().into()).keep(dismissed);
        self.overlays.update(cx, |overlays, cx| overlays.push(entry, cx));
    }

    /// A view on the stack asked to be dismissed: the stack pops newest
    /// first, so an entry standing over the view (a menu opened over a
    /// sheet) closes before the view does — Escape and a click away close
    /// the newest overlay wherever the keyboard or the pointer is. A view no
    /// entry holds any more (closed already) changes nothing.
    pub(crate) fn dismiss_overlay_view(&mut self, view: gpui::EntityId, cx: &mut Context<Self>) {
        if self.overlays.read(cx).stack.holds_view(view) {
            self.pop_overlay_entry(cx);
        }
    }

    /// Close the top entry: notify the view that draws it, run its own
    /// close hook. `false` when nothing was open.
    pub(crate) fn pop_overlay_entry(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.overlays.update(cx, |overlays, cx| overlays.pop(cx)) else {
            return false;
        };
        let crate::overlay_stack::Entry { layer, owner, on_close, .. } = entry;
        if let Some(owner) = owner {
            let app: &mut gpui::App = cx;
            app.notify(owner);
        }
        // The stack's own notify (it popped) redraws the root, and with it
        // whatever the close hook changed here.
        if let Some(on_close) = on_close {
            on_close(layer, self, cx);
        }
        true
    }

    /// ⌃C, and Escape on an empty composer: stop and retract.
    /// Q4b: a no-op while a provider switch is pending — the running turn,
    /// if any, belongs to the hidden session, and ⌃C must not reach it.
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.interrupt(cx)));
        }
    }

    /// Escape: close the newest overlay (plan 4.6: every dialog, palette,
    /// menu, popover, sheet and inline rename is an entry on the overlay
    /// stack, so this function names none of them), and otherwise go back
    /// one step on what stands: the Settings and Skills pages, the right
    /// pane's view stack, the transcript's selection and a card's field —
    /// and last, stop the running turn if the composer is empty (spec §3.9).
    /// The ratchet in `tests/overlay_stack.rs` pins what it may call.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pop_overlay(window, cx) {
            return;
        }
        // The Settings page is the first thing Escape takes back:
        // recording → search → sub-page up → close+restore (§1.3).
        if self.settings_open(cx) {
            self.settings_escape(cx);
            return;
        }
        // The Skills page is the next thing Escape takes back, from any
        // focus the window's own Cancel reaches (V1): the page's own
        // `SkillsClose` only fires with focus inside the page, so from the
        // dock or the sidebar nothing left it. One layer per press, through
        // the same `close_skills` the page's key takes — a dialog, then the
        // menu, then the page. A text field that consumes Escape never
        // reaches here, so field editing is untouched.
        if self.skills_open(cx) {
            self.close_skills(cx);
            return;
        }
        // An open file preview is the next thing Escape takes back: back
        // to the tree, keeping the selected marker and the scroll offset.
        if let Some((root, _)) = self.right_project(cx) {
            if self.with_right_pane(cx, |pane, cx| pane.close_file_preview_for(&root, cx)) {
                let focus = self.right_pane.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                return;
            }
        }
        // Then a pushed full output or image on the screen the pane shows
        // (docs/43 §2.9–2.10: ←, Esc and ⌘[ go back).
        if crate::layout::Layout::get(cx).right_open {
            let screen = crate::right::screen_index(crate::layout::right_kind(crate::layout::Layout::get(cx)));
            if self.right_pane.update(cx, |pane, cx| pane.pop_right_view(screen, cx)) {
                return;
            }
        }
        // A transcript text selection is the next thing Escape takes back.
        if let Some(view) = self.active.clone() {
            if view.update(cx, |view, cx| view.clear_selection(cx)) {
                return;
            }
        }
        // A card's open field is the next thing Escape takes back, before it
        // reaches for the running turn. It is no overlay: the field is part
        // of the card in the transcript, and closing it is the card's own
        // state, like the selection above.
        let field = self
            .active
            .clone()
            .map(|view| view.update(cx, |view, cx| view.close_card_field(cx)))
            .unwrap_or(false);
        if field {
            // The view redrew itself; the composer's focus rides the settle.
            self.arm_composer_focus(cx);
            return;
        }
        // The first run's regions strip (docs/82 8.7) is the last thing
        // Escape takes back before the running turn: it is no overlay, but
        // it stands over the column until it is dismissed.
        if self.active.is_some() && self.dismiss_regions_strip(cx) {
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
    use crate::layout::LayoutGlobal as _;
    use crate::sidebar::SessionRow as _;
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
            theme: Some(aui_tokens::ThemeKind::Dark),
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
    /// lets the window's focus logic run (the root's settle, and focus lost
    /// at the end of the draw, task 4.9): the open palette renders through
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_open));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_palette(PaletteKind::Commands, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            h.overlays.update(cx, |o, cx| o.close_palette(cx));
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let root = state.2.clone();
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| {
                    // The durable record names a minted session …
                    list.provider_sessions.insert(
                        "minted-1".to_owned(),
                        crate::provider_sessions::ProviderSessionRecord {
                            provider: "codex".to_owned(),
                            session_id: "minted-1".to_owned(),
                            workspace: None,
                            created_ms: 0,
                            updated_ms: 0,
                            turns: 0,
                            origin: None,
                        },
                    );
                });
                // … while the bridge map names the pre-ack request id.
                h.register_terminal_session("cmd-open", root.clone(), "codex", cx);
                h.register_terminal_session("cc-bridge", root.clone(), "claude-code", cx);
                h.register_terminal_session("m-bridge", root, "muse", cx);
            });
        });
        vc.update(|_, cx| {
            let hint = |h: &Harness, origin: Option<&str>| {
                let provider = h.terminal_tab_provider(origin, cx);
                crate::terminal::TerminalDock::hint_name(provider)
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
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view = cx.new(|cx| {
                    crate::session::SessionView::new("s-1".to_owned(), host, window, cx)
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).states.is_empty()),
            "boot draws no webview"
        );
        // A session switch with no Browser state: the activate tail —
        // focus the composer, restore the (absent) pane state.
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.window_sync.focus_composer = true;
                h.restore_right_for_session("s-1", cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).states.is_empty()),
            "a switch with no Browser state creates no webview"
        );
        restore_state(state);
    }

    /// Owner 9 Nov: a person's tab closed by hand (⌘W, ✕) must release its
    /// page — the agent tools' registry kept it alive, so the closed page
    /// never parked its native view and stayed on screen over the next tab.
    #[gpui::test]
    fn closing_a_tab_by_hand_releases_its_page(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browser-close-release");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let (id, weak) = vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                // Load the saved tabs first, as the app does before any tab
                // exists: `close_tab` loads them on first use otherwise,
                // replacing the tab opened here.
                h.ensure_tabs_loaded(cx);
                let space = h.browser.read(cx).browse_space();
                let id = h.browser.update(cx, |bv, _| bv.tabs.open(space, crate::browser::tabs::Owner::Person, "about:blank", None));
                let page = h.ensure_tab_page("home", id, window, cx);
                (id, page.downgrade())
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.close_tab("home", id, window, cx)));
        draw_shell(&mut *vc, &baaz);
        assert!(weak.upgrade().is_none(), "the closed tab's page is still held, so its view stays on screen");
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
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
                h.window_sync.focus_composer = true;
                h.restore_right_for_session("s-1", cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        let url_focus = vc.update(|_, cx| {
            baaz.read(cx)
                .browser.read(cx).session_page("s-1")
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
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
                .browser.read(cx).session_page("s-1")
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let start = vc.update(|_, cx| crate::layout::Layout::get(cx).right_open);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
            !start,
            "one toggle flips the pane"
        );
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
            start,
            "two toggles return to the start"
        );
        restore_state(state);
    }

    /// B11b: while the Settings page stands open, pane/terminal toggles
    /// and pane-kind commands are ignored with no state change — and write
    /// nothing, neither `layout.json` state nor a per-session record.
    #[gpui::test]
    fn pane_toggles_while_settings_open_change_nothing(cx: &mut gpui::TestAppContext) {
        use crate::overlays::PaletteKind;
        let state = hermetic_state("settings-pane-guard");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("general", cx)));
        assert!(vc.update(|_, cx| baaz.read(cx).settings_open(cx)));
        let (right_open, terminal_open, overrides) = vc.update(|_, cx| {
            let h = baaz.read(cx);
            (crate::layout::Layout::get(cx).right_open, crate::layout::Layout::get(cx).terminal_open, h.session_list.read(cx).overrides.clone())
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_terminal(window, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Changes, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.new_terminal(window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.open_terminal_tab(None, window, cx)));
        let refused = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_right("git", cx)));
        assert!(refused.is_err(), "the right step fails while Settings stands open");
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(crate::layout::Layout::get(cx).right_open, right_open, "toggle_right changed hidden state");
            assert_eq!(crate::layout::Layout::get(cx).terminal_open, terminal_open, "a dock command changed hidden state");
            assert!(h.session_list.read(cx).overrides == overrides, "a per-session pane record was written");
            assert!(h.settings_open(cx), "a guarded command closed Settings");
        });
        // The matching palette action is ignored the same way: it neither
        // acts nor exits the page.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_palette(PaletteKind::Commands, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                use crate::overlays::Command;
                let rows = h.palette_rows(PaletteKind::Commands, cx);
                let id = rows
                    .iter()
                    .find(|(id, _, _)| {
                        Command::parse(id).is_some_and(|command| command.is_pane_or_terminal())
                    })
                    .map(|(id, _, _)| id.clone())
                    .expect("the palette lists a pane/terminal command");
                h.run_palette_row_for_test(PaletteKind::Commands, id, window, cx);
            })
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(crate::layout::Layout::get(cx).terminal_open, terminal_open, "the palette action flipped the dock");
            assert!(h.settings_open(cx), "the ignored palette action exited Settings");
        });
        restore_state(state);
    }

    /// Task 4.4: the terminal dock is its own region entity. Picking a tab
    /// on its strip lands on the dock's host, which notifies (the dock draws
    /// it) — the root is never notified. (No frame is drawn here: the grid's first
    /// layout resizes its pty, which redraws inside the test draw and trips
    /// gpui's stale-arena panic; the root composes the dock uncached, so a
    /// notified dock re-renders on the next frame.)
    #[gpui::test]
    fn a_tab_pick_lands_on_the_dock_without_notifying_the_root(cx: &mut gpui::TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;
        let state = hermetic_state("dock-entity");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        // Two scripted tabs on the project (FakePty: no shell runs).
        for title in ["one", "two"] {
            let opened = vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_terminal_dock(title, window, cx)));
            assert!(opened.is_ok(), "the scripted dock opens: {opened:?}");
        }
        let dock = vc.update(|_, cx| baaz.read(cx).terminal_dock.clone());
        let root = vc.update(|_, cx| baaz.read(cx).current_project(cx).map(|project| project.root.clone())).expect("the hermetic workspace is the project");
        let tabs = vc.update(|_, cx| dock.read(cx).host().read(cx).tabs_for(&root).iter().map(|tab| tab.id.clone()).collect::<Vec<_>>());
        assert_eq!(tabs.len(), 2, "two tabs on the project");
        let active = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| dock.read(cx).host().read(cx).active_for(&root).map(|tab| tab.id.clone()));
        assert_eq!(active(vc).as_deref(), Some(tabs[1].as_str()), "the last open is active");
        vc.run_until_parked();
        // The host owns the tabs and notifies for the pick (docs/82 6.5);
        // the dock, which draws the host, redraws with it.
        let host = vc.update(|_, cx| dock.read(cx).host().clone());
        let (dock_notes, root_notes) = (Rc::new(Cell::new(0usize)), Rc::new(Cell::new(0usize)));
        let _watch = vc.update(|_, cx| {
            let (d, r) = (dock_notes.clone(), root_notes.clone());
            (cx.observe(&host, move |_, _| d.set(d.get() + 1)), cx.observe(&baaz, move |_, _| r.set(r.get() + 1)))
        });
        vc.update(|_, cx| dock.update(cx, |dock, cx| dock.activate_tab(0, cx)));
        vc.run_until_parked();
        assert_eq!(active(vc).as_deref(), Some(tabs[0].as_str()), "the pick landed on the host");
        assert!(dock_notes.get() > 0, "the pick notifies the host the dock draws");
        assert_eq!(root_notes.get(), 0, "the root is not notified for the dock's own state");
        restore_state(state);
    }

    /// docs/82 8.1: a window over the hermetic workspace with one scripted
    /// terminal tab (FakePty: no shell runs, so nothing below spawns one).
    type TestState = (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf);

    fn terminal_window<'a>(cx: &'a mut gpui::TestAppContext, name: &str) -> (TestState, &'a mut gpui::VisualTestContext, gpui::Entity<Harness>, PathBuf) {
        let state = hermetic_state(name);
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let opened = vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_terminal_dock("zsh", window, cx)));
        assert!(opened.is_ok(), "the scripted tab opens: {opened:?}");
        let root = vc.update(|_, cx| baaz.read(cx).current_project(cx).map(|project| project.root.clone())).expect("the hermetic workspace is the project");
        (state, vc, baaz, root)
    }

    fn tab_ids(vc: &mut gpui::VisualTestContext, baaz: &gpui::Entity<Harness>, root: &std::path::Path) -> Vec<String> {
        vc.update(|_, cx| baaz.read(cx).terminal_host(cx).read(cx).tabs_for(root).iter().map(|tab| tab.id.clone()).collect())
    }

    /// docs/82 8.1: ⌃` follows the terminal's place — the dock flips at the
    /// bottom; in the pane the pane opens on the terminal, and closes when
    /// the terminal is its top view.
    #[gpui::test]
    fn the_terminal_toggle_follows_its_place(cx: &mut gpui::TestAppContext) {
        let (state, vc, baaz, root) = terminal_window(cx, "terminal-toggle-place");
        let layout = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| crate::layout::Layout::get(cx).clone());
        assert!(layout(vc).dock_open(), "the scripted tab shows in the dock");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_terminal(window, cx)));
        assert!(!layout(vc).terminal_shown(), "⌃` shuts the dock");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_terminal(window, cx)));
        assert!(layout(vc).dock_open(), "and opens it");
        // In the pane.
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.move_terminal(crate::layout::TerminalPlace::Pane, window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_terminal(window, cx)));
        let now = layout(vc);
        assert!(!now.right_open && !now.terminal_shown() && !now.dock_open(), "⌃` closes the pane the terminal tops");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_terminal(window, cx)));
        let now = layout(vc);
        assert!(now.right_open && crate::layout::right_kind(&now) == crate::layout::RightKind::Terminal, "and opens it on the terminal");
        assert!(!now.dock_open(), "never in two places");
        assert_eq!(tab_ids(vc, &baaz, &root).len(), 1, "no toggle opened another tab");
        restore_state(state);
    }

    /// docs/82 8.1: moving the terminal — through the header control's own
    /// event — keeps the same tab and pty, and one place at a time.
    #[gpui::test]
    fn moving_the_terminal_keeps_its_tab(cx: &mut gpui::TestAppContext) {
        let (state, vc, baaz, root) = terminal_window(cx, "terminal-move");
        let before = tab_ids(vc, &baaz, &root);
        assert_eq!(before.len(), 1);
        let session = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).terminal_host(cx).read(cx).tabs_for(&root).first().map(|tab| tab.session.entity_id()))
        };
        let pty = session(vc);
        let dock = vc.update(|_, cx| baaz.read(cx).terminal_dock.clone());
        // The dock header's "Move terminal to the right pane".
        vc.update(|_, cx| dock.update(cx, |_, cx| cx.emit(crate::terminal::TerminalDockEvent::Move(crate::layout::TerminalPlace::Pane))));
        vc.run_until_parked();
        let now = vc.update(|_, cx| crate::layout::Layout::get(cx).clone());
        assert_eq!(now.terminal_place, crate::layout::TerminalPlace::Pane);
        assert!(now.terminal_in_pane() && !now.dock_open(), "the pane shows it and the dock closed");
        assert_eq!(tab_ids(vc, &baaz, &root), before, "the same tab, no new one");
        assert_eq!(session(vc), pty, "the same pty, not restarted");
        // The pane's "Dock terminal at the bottom".
        vc.update(|_, cx| dock.update(cx, |_, cx| cx.emit(crate::terminal::TerminalDockEvent::Move(crate::layout::TerminalPlace::Bottom))));
        vc.run_until_parked();
        let now = vc.update(|_, cx| crate::layout::Layout::get(cx).clone());
        assert!(now.dock_open() && !now.terminal_in_pane(), "back in the dock, gone from the pane");
        assert_ne!(crate::layout::right_kind(&now), crate::layout::RightKind::Terminal, "the pane's terminal view went back");
        assert_eq!(tab_ids(vc, &baaz, &root), before);
        assert_eq!(session(vc), pty);
        // (No frame is drawn: the grid's first layout resizes its pty, which
        // trips gpui's stale-arena panic in a test draw — see the tab-pick
        // test above. The captures draw both places.)
        restore_state(state);
    }

    /// docs/82 8.1: the top view taking the lower pane's kind closes the
    /// lower pane, on the session's record too; the Page never sits below.
    #[gpui::test]
    fn the_top_taking_the_lower_kind_closes_it(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("right-lower-close");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        use crate::layout::RightKind;
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.select_right(RightKind::Changes, cx)));
        let set = vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_right_lower(Some(RightKind::Files), window, cx)));
        assert_eq!(set, Ok(()));
        let refused = vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_right_lower(Some(RightKind::Browser), window, cx)));
        assert!(refused.is_err(), "the Page is top-only");
        let stored = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).session_list.read(cx).overrides.get("s-1").and_then(|meta| meta.right.clone()).map(|right| right.lower))
        };
        assert_eq!(stored(vc), Some(Some(RightKind::Files)), "the session's record holds the lower pane");
        assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).shown_lower()), Some(RightKind::Files));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.select_right(RightKind::Files, cx)));
        assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_lower), None, "the top took Files: the lower pane closed");
        assert_eq!(stored(vc), Some(None), "and the record says so");
        restore_state(state);
    }

    /// docs/82 8.1: the lower pane and its divider are per session — a
    /// switch restores each session's own, and a session that stored none
    /// shows none.
    #[gpui::test]
    fn the_lower_pane_restores_per_session(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("right-lower-restore");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        use crate::layout::RightKind;
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.select_right(RightKind::Changes, cx);
                h.set_right_lower(Some(RightKind::Tasks), window, cx).expect("Tasks sits below");
                h.set_right_split(0.9, cx);
                h.set_override(
                    "s-2",
                    |meta| meta.right = Some(crate::sessions::RightState { open: true, kind: RightKind::Files, ..Default::default() }),
                    cx,
                );
            })
        });
        assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).split()), crate::layout::SPLIT_MAX, "the divider is held to its range");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.restore_right_for_session("s-2", cx)));
        let other = vc.update(|_, cx| crate::layout::Layout::get(cx).clone());
        assert_eq!((crate::layout::right_kind(&other), other.right_lower, other.right_split), (RightKind::Files, None, None), "s-2 stored no lower pane");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.restore_right_for_session("s-1", cx)));
        let back = vc.update(|_, cx| crate::layout::Layout::get(cx).clone());
        assert_eq!((crate::layout::right_kind(&back), back.shown_lower(), back.split()), (RightKind::Changes, Some(RightKind::Tasks), crate::layout::SPLIT_MAX));
        restore_state(state);
    }

    /// Task 4.4: the Skills page is its own region entity. Its own keys —
    /// Esc peeling the Add menu, Enter moving into the detail — land on the
    /// page and notify the page alone; the root is never notified, and the
    /// page still stands open (the root reads it each frame). Fixture
    /// catalog only: no `muse skills` call runs.
    #[gpui::test]
    fn the_skills_page_peels_its_menu_without_notifying_the_root(cx: &mut gpui::TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;
        let state = hermetic_state("skills-entity");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let opened = vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_skills("add-menu", window, cx)));
        assert!(opened.is_ok(), "the fixture page opens: {opened:?}");
        vc.run_until_parked();
        let page = vc.update(|_, cx| baaz.read(cx).skills_page.clone());
        assert!(vc.update(|_, cx| page.read(cx).add_menu_open(cx)), "the fixture opens the Add menu");
        let (page_notes, root_notes) = (Rc::new(Cell::new(0usize)), Rc::new(Cell::new(0usize)));
        let _watch = vc.update(|_, cx| {
            let (p, r) = (page_notes.clone(), root_notes.clone());
            (cx.observe(&page, move |_, _| p.set(p.get() + 1)), cx.observe(&baaz, move |_, _| r.set(r.get() + 1)))
        });
        vc.update(|_, cx| page.update(cx, |page, cx| page.close_skills(cx)));
        vc.update(|_, cx| page.update(cx, |page, cx| page.skills_focus_detail(cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let shown = page.read(cx);
            assert!(!shown.add_menu_open(cx), "Esc peeled the menu");
            assert!(shown.detail_focused, "Enter moved into the detail");
            assert!(baaz.read(cx).skills_open(cx), "the page itself stays open");
            assert!(crate::region::Region::covers_page(shown, cx), "an open page covers Browse's page");
        });
        assert!(page_notes.get() > 0, "the page is notified");
        assert_eq!(root_notes.get(), 0, "the root is not notified for the page's own state");
        restore_state(state);
    }

    /// Task 4.4: the Settings page is its own region entity. Its own state
    /// — a Shortcuts row armed then disarmed by Escape, a provider card's
    /// Details, the move row's path list, the "New sessions" menu — lands
    /// on the page and notifies the page alone; the root is never notified,
    /// and the page still stands open (the root reads it each frame).
    #[gpui::test]
    fn the_settings_page_records_a_shortcut_without_notifying_the_root(cx: &mut gpui::TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;
        let state = hermetic_state("settings-entity");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let opened = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_settings("shortcuts", cx)));
        assert!(opened.is_ok(), "the page opens: {opened:?}");
        vc.run_until_parked();
        let page = vc.update(|_, cx| baaz.read(cx).settings_page.clone());
        let (page_notes, root_notes) = (Rc::new(Cell::new(0usize)), Rc::new(Cell::new(0usize)));
        let _watch = vc.update(|_, cx| {
            let (p, r) = (page_notes.clone(), root_notes.clone());
            (cx.observe(&page, move |_, _| p.set(p.get() + 1)), cx.observe(&baaz, move |_, _| r.set(r.get() + 1)))
        });
        let id = gpui::SharedString::from(crate::settings::shortcut_row_id("NewSession", Some("AuiRoot")));
        vc.update(|_, cx| page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Record, cx)));
        assert_eq!(vc.update(|_, cx| page.read(cx).recording.clone()), Some(id.to_string()), "Record arms the row");
        // Escape's first layer is the page's own: it disarms the row.
        let escaped = vc.update(|_, cx| baaz.update(cx, |h, cx| h.settings_escape(cx)));
        assert!(escaped, "Escape went to the open page");
        vc.update(|_, cx| page.update(cx, |page, cx| page.open_details(crate::providers::ProviderId::Codex, cx)));
        vc.update(|_, cx| page.update(cx, |page, cx| page.toggle_migration_paths(cx)));
        vc.update(|_, cx| page.update(cx, |page, cx| page.open_default_menu(cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let shown = page.read(cx);
            assert!(shown.recording.is_none(), "Escape disarmed the row");
            assert!(shown.details_open.contains(&crate::providers::ProviderId::Codex), "Details opened on the page");
            assert!(shown.migration_paths_expanded, "the path list unfolded on the page");
            assert!(shown.default_menu_open(cx), "the picker's menu opened on the page");
            assert!(baaz.read(cx).settings_open(cx), "the page itself stays open");
            assert!(crate::region::Region::covers_page(shown, cx), "an open page covers Browse's page");
            assert_eq!(crate::region::Region::title(shown, cx).as_ref(), "Shortcuts");
        });
        assert!(page_notes.get() > 0, "the page is notified");
        assert_eq!(root_notes.get(), 0, "the root is not notified for the page's own state");
        restore_state(state);
    }

    /// A page the test drives from outside, the way a link moves a real
    /// one: what it reports is pushed into `events` and drained by the
    /// page's own poll. Nothing in Baaz asks it to navigate.
    struct LinkPage {
        events: std::rc::Rc<std::cell::RefCell<Vec<aui_webview::WebEvent>>>,
    }

    impl aui_webview::WebBackend for LinkPage {
        fn navigate(&mut self, _url: &str) {}
        fn back(&mut self) {}
        fn forward(&mut self) {}
        fn reload(&mut self) {}
        fn eval(&mut self, _js: &str) {}
        fn set_annotate(&mut self, _on: bool) {}
        fn poll_events(&mut self) -> Vec<aui_webview::WebEvent> {
            std::mem::take(&mut *self.events.borrow_mut())
        }
    }

    /// A scripted page the test keeps a handle on: every call reaches the
    /// shared fake, so the test can move it (a `pushState`) and read what
    /// Baaz asked of it (zoom, permission answers).
    pub(crate) struct SharedPage(pub(crate) std::rc::Rc<std::cell::RefCell<aui_webview::FakeWebBackend>>);

    impl aui_webview::WebBackend for SharedPage {
        fn navigate(&mut self, url: &str) {
            self.0.borrow_mut().navigate(url);
        }
        fn back(&mut self) {
            self.0.borrow_mut().back();
        }
        fn forward(&mut self) {
            self.0.borrow_mut().forward();
        }
        fn reload(&mut self) {
            self.0.borrow_mut().reload();
        }
        fn eval(&mut self, js: &str) {
            self.0.borrow_mut().eval(js);
        }
        fn set_annotate(&mut self, on: bool) {
            self.0.borrow_mut().set_annotate(on);
        }
        fn poll_events(&mut self) -> Vec<aui_webview::WebEvent> {
            self.0.borrow_mut().poll_events()
        }
        fn can_go_back(&self) -> bool {
            self.0.borrow().can_go_back()
        }
        fn can_go_forward(&self) -> bool {
            self.0.borrow().can_go_forward()
        }
        fn set_zoom(&mut self, percent: u16) {
            self.0.borrow_mut().set_zoom(percent);
        }
        fn supports(&self, feature: aui_webview::BackendFeature) -> bool {
            self.0.borrow().supports(feature)
        }
        fn answer_permission(&mut self, id: u64, decision: aui_webview::PermissionDecision) {
            self.0.borrow_mut().answer_permission(id, decision);
        }
    }

    /// A Browse tab on a [`SharedPage`] at `url`: the harness, the test
    /// window, the tab and the fake behind its page.
    pub(crate) fn browse_tab_on_shared_page(
        vc: &mut gpui::VisualTestContext,
        state_dir: &std::path::Path,
        url: &str,
    ) -> (gpui::Entity<Harness>, crate::browser::TabId, std::rc::Rc<std::cell::RefCell<aui_webview::FakeWebBackend>>) {
        let baaz = vc.update(|window, cx| cx.new(|cx| Harness::new(test_args(state_dir), crate::runtime::CaptureToken::default(), window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_tab_new(url, window, cx))).expect("the tab opens");
        vc.run_until_parked();
        let browser = vc.update(|_, cx| baaz.read(cx).browser.clone());
        let id = vc.update(|_, cx| browser.read(cx).browse_centre_shown(cx).expect("the new tab is the centre's"));
        let fake = std::rc::Rc::new(std::cell::RefCell::new(aui_webview::FakeWebBackend::new()));
        aui_webview::WebBackend::navigate(&mut *fake.borrow_mut(), url);
        vc.update(|_, cx| {
            let page = cx.new(|cx| aui_webview::WebviewState::new(Box::new(SharedPage(fake.clone())), cx));
            browser.update(cx, |browser, cx| browser.adopt_page(id, page, cx));
        });
        vc.run_until_parked();
        (baaz, id, fake)
    }

    /// docs/82 8.6: Back and Forward are the engine's. A page that moved
    /// itself (`pushState`: Baaz never navigated there) is one step back,
    /// and Forward is offered because the engine says so — the URL replay
    /// this replaced navigated afresh and lost the forward entry.
    #[gpui::test]
    fn back_and_forward_step_the_engines_own_history(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-engine-history");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, id, fake) = browse_tab_on_shared_page(vc, &state.2, "https://a.example/");
        fake.borrow_mut().script_push_state("https://a.example/#step-2");
        vc.executor().advance_clock(std::time::Duration::from_millis(200));
        vc.run_until_parked();
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.browse_go_back(cx)));
        vc.executor().advance_clock(std::time::Duration::from_millis(200));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let page = h.browse_centre_state(cx).expect("the tab's page");
            assert_eq!(page.read(cx).url().as_ref(), "https://a.example/", "back is the page's own entry");
            assert!(page.read(cx).can_go_forward(), "forward comes from the engine");
            assert_eq!(h.browser.read(cx).tabs.get(id).map(|t| t.url.clone()).as_deref(), Some("https://a.example/"), "the tab follows its page");
        });
        assert_eq!(fake.borrow().url().as_ref(), "https://a.example/");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.browse_go_forward(cx)));
        vc.executor().advance_clock(std::time::Duration::from_millis(200));
        vc.run_until_parked();
        assert_eq!(fake.borrow().url().as_ref(), "https://a.example/#step-2", "forward walked the engine's entry");
        restore_state(state);
    }

    /// docs/82 8.6: "Always allow" keeps the answer for the site and kind,
    /// so the next ask of that kind there answers itself with no strip;
    /// "Always block" keeps a no the same way; a kind never answered still
    /// asks. (The run-only "while visiting" grant this replaced asked again
    /// every launch.)
    #[gpui::test]
    fn always_answers_are_kept_and_answer_the_next_ask(cx: &mut gpui::TestAppContext) {
        use aui_webview::{PermissionDecision, PermissionKind};
        let state = hermetic_state("browse-site-permissions");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, _id, fake) = browse_tab_on_shared_page(vc, &state.2, "https://cam.example/call");
        // The test window draws no frame, so the page counts as hidden and
        // its asks notify nobody: run the reconcile a shown page's change
        // would run.
        let browser = vc.update(|_, cx| baaz.read(cx).browser.clone());
        let settle = |vc: &mut gpui::VisualTestContext| {
            vc.executor().advance_clock(std::time::Duration::from_millis(200));
            vc.run_until_parked();
            vc.update(|_, cx| browser.update(cx, |browser, cx| browser.reconcile_pages(cx)));
            vc.run_until_parked();
        };
        fake.borrow_mut().script_permission(1, "https://cam.example", vec![PermissionKind::Camera]);
        fake.borrow_mut().script_permission(2, "https://cam.example", vec![PermissionKind::Microphone]);
        settle(vc);
        assert!(fake.borrow().answers().is_empty(), "a first ask waits for the person");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.answer_shown_permission(1, crate::browser::PermissionAnswer::AlwaysAllow, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.answer_shown_permission(2, crate::browser::PermissionAnswer::AlwaysBlock, cx)));
        settle(vc);
        vc.update(|_, cx| {
            let sites = baaz.read(cx).browser.read(cx).data.read(cx).sites().clone();
            assert_eq!(sites.answer("https://cam.example", "camera"), Some(baaz_browser_core::sites::Answer::Allow));
            assert_eq!(sites.answer("https://cam.example", "microphone"), Some(baaz_browser_core::sites::Answer::Block));
        });
        // The site asks again: answered from what was kept, no strip.
        fake.borrow_mut().script_permission(3, "https://cam.example", vec![PermissionKind::Camera]);
        fake.borrow_mut().script_permission(4, "https://cam.example", vec![PermissionKind::Camera, PermissionKind::Microphone]);
        fake.borrow_mut().script_permission(5, "https://cam.example", vec![PermissionKind::Geolocation]);
        settle(vc);
        let answers = fake.borrow().answers().to_vec();
        assert!(answers.contains(&(3, PermissionDecision::Allow)), "the kept allow answered: {answers:?}");
        assert!(answers.contains(&(4, PermissionDecision::Deny)), "one kept block blocks the ask: {answers:?}");
        assert!(!answers.iter().any(|(id, _)| *id == 5), "a kind never answered still asks: {answers:?}");
        restore_state(state);
    }

    /// docs/82 8.6: zoom is kept per site. The shown page's zoom steps and
    /// is stored for its site; another site's page opens at 100 %, and
    /// coming back to the site puts its zoom back.
    #[gpui::test]
    fn zoom_is_kept_per_site_and_set_on_arrival(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-site-zoom");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, _id, fake) = browse_tab_on_shared_page(vc, &state.2, "https://a.example/one");
        // The test window draws no frame, so the page counts as hidden and
        // its asks notify nobody: run the reconcile a shown page's change
        // would run.
        let browser = vc.update(|_, cx| baaz.read(cx).browser.clone());
        let settle = |vc: &mut gpui::VisualTestContext| {
            vc.executor().advance_clock(std::time::Duration::from_millis(200));
            vc.run_until_parked();
            vc.update(|_, cx| browser.update(cx, |browser, cx| browser.reconcile_pages(cx)));
            vc.run_until_parked();
        };
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.zoom_shown_page(1, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.zoom_shown_page(1, cx)));
        settle(vc);
        assert_eq!(fake.borrow().zoom_calls().last(), Some(&125));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(h.browser.read(cx).data.read(cx).sites().zoom("https://a.example"), Some(125), "kept for the site");
            assert_eq!(h.shown_page_zoom(cx), Some(125), "the page menu's Zoom row reads it");
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            if let Some(page) = h.browse_centre_state(cx) {
                page.update(cx, |page, _| page.navigate("https://b.example/"));
            }
        }));
        settle(vc);
        assert_eq!(fake.borrow().zoom_calls().last(), Some(&100), "another site opens at 100 %");
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            if let Some(page) = h.browse_centre_state(cx) {
                page.update(cx, |page, _| page.navigate("https://a.example/two"));
            }
        }));
        settle(vc);
        assert_eq!(fake.borrow().zoom_calls().last(), Some(&125), "the site's zoom comes back with it");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.zoom_shown_page(0, cx)));
        settle(vc);
        vc.update(|_, cx| assert_eq!(baaz.read(cx).browser.read(cx).data.read(cx).sites().zoom("https://a.example"), None, "100 % keeps nothing"));
        restore_state(state);
    }

    /// docs/82 8.6: the agent cursor sits where the session's page tool
    /// last acted on the page shown — in the page's coordinates at its zoom
    /// — named by the session, and goes when the person takes the wheel.
    #[cfg(feature = "probe")]
    #[gpui::test]
    fn the_agent_cursor_follows_the_tools_last_point(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-agent-cursor");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, _id, fake) = browse_tab_on_shared_page(vc, &state.2, "https://shop.example/checkout");
        assert!(vc.update(|_, cx| baaz.read(cx).agent_cursor_spot(cx)).is_none(), "nobody acts: no cursor");
        fake.borrow_mut().script_zoom(125);
        vc.executor().advance_clock(std::time::Duration::from_millis(200));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let page = baaz.read(cx).browse_centre_state(cx).expect("the tab's page");
            baaz.read(cx).terminal_service.script_driving("cursor-session", &page, "clicking “Next”", (100.0, 40.0));
        });
        let spot = vc.update(|_, cx| baaz.read(cx).agent_cursor_spot(cx)).expect("a tool acts on the page shown");
        assert_eq!((f32::from(spot.0.x), f32::from(spot.0.y)), (125.0, 50.0), "the point, at the page's zoom");
        assert_eq!(spot.1, "An agent is acting here", "named by the session, never a provider");
        vc.update(|_, cx| baaz.read(cx).terminal_service.take_over("cursor-session"));
        assert!(vc.update(|_, cx| baaz.read(cx).agent_cursor_spot(cx)).is_none(), "the person drives: no cursor");
        restore_state(state);
    }

    /// docs/82 8.6: Bookmarks and History open over the Browse centre
    /// (the native page may not show under them), ⌘Y shuts the shelf it
    /// opened, and a row opens its page in a new tab and leaves the page.
    #[gpui::test]
    fn the_history_page_covers_the_centre_and_opens_rows_in_a_tab(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-library");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, _id, _fake) = browse_tab_on_shared_page(vc, &state.2, "https://a.example/");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_library(crate::browser::library::Shelf::History, window, cx)));
        vc.update(|window, cx| {
            let h = baaz.read(cx);
            assert!(h.library_open(cx), "the page stands open");
            assert!(!h.page_covers(window, cx).centre_allowed, "the native page hides under it");
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.on_library_event(crate::browser::library::LibraryEvent::Open("https://b.example/".into()), window, cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(!h.library_open(cx), "opening a row leaves the page");
            let shown = h.browser.read(cx).browse_centre_shown(cx).and_then(|id| h.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()));
            assert_eq!(shown.as_deref(), Some("https://b.example/"), "in a new tab, shown");
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_library(crate::browser::library::Shelf::Bookmarks, window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_library(crate::browser::library::Shelf::Bookmarks, window, cx)));
        assert!(!vc.update(|_, cx| baaz.read(cx).library_open(cx)), "the same key shuts the shelf it opened");
        restore_state(state);
    }

    /// Task 4.5b: the Browse view follows its pages itself. A person tab's
    /// page moves on its own (a link: nothing in Baaz asks), and with no
    /// window frame drawn — the root never renders in this window, and the
    /// frame's occlusion never ran — the tab takes the page's URL and
    /// title, and the visit reaches history. The view is notified (it redraws); the root is not.
    #[gpui::test]
    fn a_page_change_reaches_the_tabs_and_history_with_no_frame(cx: &mut gpui::TestAppContext) {
        use aui_webview::WebEvent;
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        let state = hermetic_state("browser-view-reconcile");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_tab_new("https://a.example/", window, cx)))
            .expect("the tab opens");
        vc.run_until_parked();
        let browser = vc.update(|_, cx| baaz.read(cx).browser.clone());
        let id = vc.update(|_, cx| browser.read(cx).browse_centre_shown(cx).expect("the new tab is the centre's"));
        assert_eq!(
            vc.update(|_, cx| browser.read(cx).data.read(cx).history_len()),
            1,
            "a page built at its URL reaches history on adoption"
        );
        // The tab's page, driven from outside from here on.
        let events: Rc<RefCell<Vec<WebEvent>>> = Rc::default();
        vc.update(|_, cx| {
            let page = cx.new(|cx| aui_webview::WebviewState::new(Box::new(LinkPage { events: events.clone() }), cx));
            browser.update(cx, |browser, cx| browser.adopt_page(id, page, cx));
        });
        vc.run_until_parked();
        let (view_notes, root_notes) = (Rc::new(Cell::new(0usize)), Rc::new(Cell::new(0usize)));
        let _watch = vc.update(|_, cx| {
            let (v, r) = (view_notes.clone(), root_notes.clone());
            (cx.observe(&browser, move |_, _| v.set(v.get() + 1)), cx.observe(&baaz, move |_, _| r.set(r.get() + 1)))
        });
        events.borrow_mut().extend([
            WebEvent::Url("https://b.example/next".to_owned()),
            WebEvent::Title("Next".to_owned()),
        ]);
        vc.executor().advance_clock(std::time::Duration::from_millis(200));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = browser.read(cx);
            let tab = view.tabs.get(id).expect("the tab stays");
            assert_eq!((tab.url.as_str(), tab.title.as_str()), ("https://b.example/next", "Next"), "the tab follows its page");
            assert_eq!(view.data.read(cx).history_len(), 2, "the visit reached history");
            assert!(view.centre_page_rect.is_none(), "no frame drew: the occlusion never ran");
            assert_eq!(view.occlusion.shown(), crate::browser::PageShow::default(), "nor decided anything");
        });
        assert!(view_notes.get() > 0, "the page change redraws the view");
        assert_eq!(root_notes.get(), 0, "the root is not notified for the view's own state");
        restore_state(state);
    }

    /// B11b: opening the palette — and cancelling it — keeps the Settings
    /// page exactly where it was.
    #[gpui::test]
    fn palette_open_and_cancel_keeps_settings(cx: &mut gpui::TestAppContext) {
        use crate::overlays::PaletteKind;
        let state = hermetic_state("settings-palette-keep");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("sidebar", cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_palette(PaletteKind::Commands, cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.settings_open(cx), "opening the palette exited Settings");
            assert_eq!(h.settings_section(cx), "sidebar");
        });
        // Cancelling (Escape's path: pop the overlay stack) keeps it.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.pop_overlay(window, cx);
            })
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.settings_open(cx), "cancelling the palette exited Settings");
            assert_eq!(h.settings_section(cx), "sidebar");
        });
        restore_state(state);
    }

    /// B11b: a session switch exits Settings with no restore — and ⌘,
    /// spelled as the empty target, reopens the last-visited section.
    #[gpui::test]
    fn a_session_switch_exits_settings_and_reopen_restores_last_section(
        cx: &mut gpui::TestAppContext,
    ) {
        let state = hermetic_state("settings-switch-exit");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("sidebar", cx)));
        assert!(vc.update(|_, cx| baaz.read(cx).settings_open(cx)));
        // A second session landing is the switch: build it the way the
        // scripted chrome does, then run the real choke point.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let host = crate::session::SessionHost {
                    provider_id: "echo".to_owned(),
                    workspace: state.2.to_string_lossy().into_owned(),
                    overlays: h.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view = cx.new(|cx| crate::session::SessionView::new("s-2".to_owned(), host, window, cx));
                h.activate(view, false, window, cx);
            })
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(!h.settings_open(cx), "a session switch left Settings open");
            assert_eq!(h.settings_page.read(cx).state.saved_active, None, "a navigation exit keeps a restore record");
        });
        // ⌘, reopens the last-visited section (Sidebar), not General.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("", cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.settings_open(cx));
            assert_eq!(h.settings_section(cx), "sidebar", "⌘, did not reopen the last section");
        });
        restore_state(state);
    }

    /// B11b: opening Settings moves the keyboard onto the surface (so ⌘F
    /// and nav work at once); closing moves it back to the composer.
    #[gpui::test]
    fn settings_focus_moves_in_and_back_out(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("settings-focus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        draw_shell(&mut *vc, &baaz);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("general", cx)));
        draw_shell(&mut *vc, &baaz);
        let surface = vc.update(|_, cx| baaz.read(cx).settings_page.read(cx).focus.clone());
        assert_eq!(
            vc.update(|window, cx| window.focused(cx)),
            Some(surface),
            "opening Settings did not move focus onto the surface"
        );
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.close_settings(cx)));
        draw_shell(&mut *vc, &baaz);
        let composer = vc.update(|_, cx| {
            baaz.read(cx).active.clone().expect("a session is open").update(cx, |view, cx| {
                view.composer_focus_handle(cx)
            })
        });
        assert_eq!(
            vc.update(|window, cx| window.focused(cx)),
            Some(composer),
            "closing Settings did not refocus the composer"
        );
        restore_state(state);
    }

    /// B7: opening or closing the right pane does no session-list or search
    /// work and no synchronous store write. Pane state is not
    /// searchable, so the toggle persists through the cheap debounced
    /// off-thread write only: the search-rebuild counter stays put and no
    /// session facts are stored synchronously afterwards. Opening on Browser
    /// also prepares the webview ahead of the animation, off the render
    /// path: the state exists before any frame draws it.
    #[gpui::test]
    fn toggling_the_right_pane_does_no_search_or_sessions_write(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("toggle-cheap");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        Harness::reset_search_rebuild_count();
        assert!(
            !crate::sessions::stored(),
            "the hermetic store starts with no session files"
        );
        // Open on Browser: the webview is prepared on the toggle, before
        // any frame — never as a render side effect.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Browser, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).session_has_page("s-1")),
            "opening on Browser prepares the webview off the render path"
        );
        assert_eq!(
            Harness::search_rebuild_count(),
            0,
            "opening the pane must not rebuild the search index"
        );
        assert!(
            !crate::sessions::stored(),
            "opening the pane must not synchronously write the session's row"
        );
        // The in-memory state still lands at once: the cheap path defers
        // only the disk write, never the state itself.
        assert!(
            vc.update(|_, cx| baaz
                .read(cx)
                .session_list
                .read(cx)
                .overrides
                .get("s-1")
                .and_then(|meta| meta.right.clone())
                .is_some_and(|right| right.open && right.kind == crate::layout::RightKind::Browser)),
            "the pane state lands in memory on open"
        );
        // Close again: still no search work and still no synchronous write.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(!vc.update(|_, cx| crate::layout::Layout::get(cx).right_open), "the toggle closes the pane");
        assert_eq!(
            Harness::search_rebuild_count(),
            0,
            "closing the pane must not rebuild the search index"
        );
        assert!(
            !crate::sessions::stored(),
            "closing the pane must not synchronously write the session's row"
        );
        restore_state(state);
    }

    /// B7fix: the debounced disk write eventually persists the per-session
    /// pane state. The toggle lands it in memory at once with no synchronous
    /// write; once the debounce elapses, the off-thread write
    /// carries the same open/kind the restore path reads.
    #[gpui::test]
    fn debounced_right_save_eventually_persists_per_session_state(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("right-debounced-persist");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Changes, cx)));
        assert!(
            !crate::sessions::stored(),
            "the toggle itself still writes nothing synchronously"
        );
        // Past the store's 750 ms delay (real time: the store's writer is
        // a thread, not the test executor), then whatever is still queued
        // lands.
        std::thread::sleep(std::time::Duration::from_millis(850));
        crate::sessions::flush();
        let mut disk = crate::sessions::read();
        for _ in 0..10 {
            if disk.get("s-1").and_then(|meta| meta.right.clone()).is_some() {
                break;
            }
            vc.run_until_parked();
            disk = crate::sessions::read();
        }
        assert_eq!(
            disk.get("s-1").and_then(|meta| meta.right.clone()),
            Some(crate::sessions::RightState {
                open: true,
                kind: crate::layout::RightKind::Changes,
                files_preview: None,
                files_selected: None,
                files_expanded: Vec::new(),
                browser_url: None,
                lower: None,
                split: None,
            }),
            "the debounced write persists what the toggle stored in memory"
        );
        restore_state(state);
    }

    /// B7fix2: the Browser webview is constructed by the open action, not
    /// by render — and never twice for one session. The `browser_builds`
    /// spy counts constructions, never reuses: one open action builds
    /// before any frame, frames reuse, and closing plus reopening builds
    /// nothing more. In the shipped app the same action builds the real
    /// webview through the windowed prewarm; render only ever reuses.
    #[gpui::test]
    fn opening_browser_constructs_once_before_render(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browser-construct-once");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        // The open action builds, with no frame drawn yet.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Browser, cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).browser_builds),
            1,
            "the open action constructs the webview before any frame"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).session_has_page("s-1")),
            "the webview exists before render runs"
        );
        // Frames reuse: render never constructs.
        draw_shell(&mut *vc, &baaz);
        draw_shell(&mut *vc, &baaz);
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).browser_builds),
            1,
            "render must reuse the webview, never construct one"
        );
        // Close and reopen: still the same single webview.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Browser, cx)));
        draw_shell(&mut *vc, &baaz);
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).browser_builds),
            1,
            "one session never earns a second webview"
        );
        restore_state(state);
    }


    /// ⌘L from a page that holds the keyboard: the page lets go, so typing
    /// reaches the Field (owner 2026-11-06: it landed nowhere).
    #[gpui::test]
    fn the_field_takes_the_keyboard_from_the_page(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = hermetic_state("field-keyboard");
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        let page = vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.ensure_tabs_loaded(cx);
                let space = h.browser.read(cx).browse_space();
                let id = h.browser.update(cx, |bv, _| bv.tabs.open(space, crate::browser::Owner::Person, "https://example.com/", None));
                h.browser.update(cx, |browser, _| browser.tabs.activate(id));
                h.browse_centre_page(window, cx).expect("the shown tab has a page")
            })
        });
        // A click into the page: it holds the keyboard.
        vc.update(|_, cx| page.update(cx, |page, _| page.seed_pins(&[((1.0, 1.0), "")])));
        assert!(vc.update(|_, cx| page.read(cx).holds_keyboard()), "the click gave the page the keyboard");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.focus_browser_address(window, cx)));
        assert!(!vc.update(|_, cx| page.read(cx).holds_keyboard()), "⌘L takes the keyboard back");
        restore_state(state);
    }

    /// B2a-H4d(5): Tab remembers the completed ROW, not an index. Completing
    /// a tab row then re-ranking the list (the completion's own write does)
    /// still opens that row on Enter — and a stale highlight clamps to the
    /// rows instead of missing into nothing.
    #[gpui::test]
    fn complete_remembers_the_row_not_the_index(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = hermetic_state("complete-row");
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        // Two person tabs whose URLs match different queries.
        let (alpha, beta) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.ensure_tabs_loaded(cx);
                let space = h.browser.read(cx).browse_space();
                let alpha = h.browser.update(cx, |browser, _| browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://alpha.example/1", None));
                let beta = h.browser.update(cx, |browser, _| browser.tabs.open(space, crate::browser::Owner::Person, "https://beta.example/2", None));
                h.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                (alpha, beta)
            })
        });
        // Type "alpha", highlight its tab row, Tab: the field takes the URL
        // and the ROW is remembered.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.focus_browse_address_with("alpha", window, cx);
                h.move_omnibox(1, cx);
                h.complete_omnibox(window, cx);
            })
        });
        // Typing on re-ranks to a different row at the same index — Enter
        // still opens the completed tab, not the row now sitting there.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let input = h.browser.read(cx).browse_address.clone().expect("the field is open");
                input.update(cx, |input, cx| input.set_value("beta", window, cx));
                h.commit_browse_address(window, cx);
            })
        });
        vc.update(|_, cx| {
            let shown = baaz.read(cx).browser.read(cx).browse_centre_shown(cx).expect("a tab shows");
            assert_eq!(shown, alpha, "Enter opens the completed row, not the re-ranked index");
            assert_ne!(shown, beta, "the row now at that index does not open");
        });
        // A stale highlight clamps: with two rows and index 99, Enter opens
        // the last row (the search) instead of missing.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.focus_browse_address_with("zzz-no-match-xyz", window, cx);
                h.browser.update(cx, |browser, _| browser.browse_omnibox_selected = 99);
                h.browser.update(cx, |browser, _| browser.browse_omnibox_moved = true);
                h.browser.update(cx, |browser, _| browser.browse_completed = None);
                h.commit_browse_address(window, cx);
            })
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let id = h.browser.read(cx).browse_centre_shown(cx).expect("a tab shows");
            let url = h.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_default();
            assert!(
                url.starts_with(crate::browser::toolbar::SearchEngine::Google.url_prefix()),
                "a clamped Enter searches instead of missing: {url}"
            );
        });
        restore_state(state);
    }

    /// One sidebar for both modes (owner, 2026-10-12; reverses B2a-H6): a
    /// mode switch never opens or closes the column — the four combinations
    /// the owner listed, in both directions, with the state persisted into
    /// both slots so a relaunch into either mode agrees.
    #[gpui::test]
    fn sidebar_state_is_shared_across_modes(cx: &mut gpui::TestAppContext) {
        use crate::layout::Mode;
        let state = hermetic_state("sidebar-shared");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        macro_rules! open {
            () => {
                vc.update(|_, cx| crate::layout::Layout::get(cx).sidebar_open)
            };
        }
        macro_rules! set {
            ($mode:expr) => {
                vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode($mode, window, cx)))
            };
        }
        macro_rules! toggle {
            () => {
                vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_sidebar(cx)))
            };
        }
        assert!(open!(), "the sidebar starts open");
        // Sessions open → Browse: still open.
        set!(Mode::Browse);
        assert!(open!(), "switching to Browse keeps an open sidebar open");
        // Browse: close, then back to Sessions: still closed.
        toggle!();
        assert!(!open!(), "⌘B closes it in Browse");
        set!(Mode::Sessions);
        assert!(!open!(), "switching to Sessions keeps a closed sidebar closed");
        // Sessions closed → Browse: still closed; open there; back: open.
        set!(Mode::Browse);
        assert!(!open!(), "switching back to Browse keeps it closed");
        toggle!();
        assert!(open!(), "⌘B opens it in Browse");
        set!(Mode::Sessions);
        assert!(open!(), "switching to Sessions keeps an open sidebar open");
        // The stored state follows the live one after a toggle.
        toggle!();
        vc.update(|_, cx| assert!(!crate::layout::Layout::get(cx).sidebar_open));
        restore_state(state);
    }

    /// A round trip through Browse keeps the Sessions pane and the tab the
    /// centre shows, and Browse's own pane stands closed. A person tab opened
    /// in Browse takes the centre over a held session tab.
    #[gpui::test]
    fn a_mode_round_trip_keeps_the_pane_and_the_shown_tab(cx: &mut gpui::TestAppContext) {
        use crate::layout::{Mode, RightKind};
        let state = hermetic_state("mode-round-trip");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.select_right(RightKind::Files, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_fixture_session("Fix ledger chart", window, cx)))
            .expect("the fixture session opens");
        let carried = vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(crate::layout::Layout::get(cx).mode, Mode::Browse, "the fixture lands in Browse");
            assert!(!crate::layout::Layout::get(cx).right_open, "Browse's pane stands closed");
            let shown = h.browser.read(cx).browse_centre_shown(cx).expect("the carried tab shows");
            assert_eq!(h.browser.read(cx).browse_session_tab, Some(shown));
            shown
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(Mode::Sessions, window, cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(crate::layout::Layout::get(cx).right_open, "the Sessions pane comes back open");
            assert_eq!(crate::layout::Layout::get(cx).right_kind, Some(RightKind::Files), "on its kind");
            assert_eq!(h.browser.read(cx).browse_centre_shown(cx), None, "Sessions shows no centre page");
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(Mode::Browse, window, cx)));
        vc.update(|_, cx| {
            assert_eq!(baaz.read(cx).browser.read(cx).browse_centre_shown(cx), Some(carried), "the shown tab survives the trip");
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_tab_new("https://a.example/", window, cx)))
            .expect("the tab opens");
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert_eq!(h.browser.read(cx).browse_session_tab, None);
            let shown = h.browser.read(cx).browse_centre_shown(cx).expect("the new tab shows");
            assert_ne!(shown, carried, "the person's new tab takes the centre");
        });
        restore_state(state);
    }

    /// B2a-H6: `settings-set:` flips the search engine through the same
    /// path the Settings rows use — and an
    /// unknown key or value changes nothing.
    #[gpui::test]
    fn settings_set_step_flips_search_engine(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("settings-set-engine");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_settings_set("search-engine=kagi", cx)))
            .expect("the verb lands on Kagi");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).search_engine),
            crate::browser::toolbar::SearchEngine::Kagi,
            "the verb lands on Kagi"
        );
        let refused = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_settings_set("search-engine=altavista", cx)));
        assert!(refused.is_err(), "an unknown value fails the step");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).search_engine),
            crate::browser::toolbar::SearchEngine::Kagi,
            "an unknown value changes nothing"
        );
        let refused = vc.update(|_, cx| baaz.update(cx, |h, cx| h.step_settings_set("engine=google", cx)));
        assert!(refused.is_err(), "an unknown key fails the step");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).search_engine),
            crate::browser::toolbar::SearchEngine::Kagi,
            "an unknown key changes nothing"
        );
        restore_state(state);
    }




    /// B2a-H6: vertical tabs list down the sidebar — no strip above the
    /// page. Each drawn test boots its own window because a test draw
    /// renders the cached sidebar pane at most once: the pane paints its
    /// birth frame and the draw after that reuses it.
    #[gpui::test]
    fn vertical_tabs_list_in_the_sidebar(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-vertical-tabs");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        for url in ["https://www.wikipedia.org/", "https://docs.rs/"] {
            vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_tab_new(url, window, cx))).expect("the tab opens");
        }
        draw_shell(&mut *vc, &baaz);
        assert!(vc.debug_bounds("browse-strip").is_none(), "vertical keeps the tabs down the sidebar");
        let listed = vc.update(|_, cx| baaz.read(cx).browse_person_snaps(cx).len());
        assert!(listed >= 2, "vertical lists the opened tabs in the sidebar's model, got {listed}");
        restore_state(state);
    }

    /// Flat (docs/83 §3): the stored centre page is the centre column edge to
    /// edge — from the window's top and left (closed) or the sidebar
    /// column's edge (open) — the one rect behind placement and the
    /// mouse-down hand-back.
    #[gpui::test]
    fn centre_page_rect_is_the_flat_column(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-page-rect");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_tab_new("https://www.wikipedia.org/", window, cx))
        })
        .expect("the tab opens");
        draw_shell(&mut *vc, &baaz);
        let (open_page, width) = vc.update(|_, cx| {
            let h = baaz.read(cx);
            (h.browser.read(cx).centre_page_rect.expect("the occlusion stores the rect"), h.sidebar_width(cx))
        });
        assert_eq!(open_page.origin, gpui::point(gpui::px(width), gpui::px(0.0)), "open: flush with the sidebar column");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_sidebar(cx)));
        draw_shell(&mut *vc, &baaz);
        let closed_page =
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).centre_page_rect.expect("the occlusion stores the rect"));
        assert_eq!(closed_page.origin, gpui::point(gpui::px(0.0), gpui::px(0.0)), "closed: the window's corner");
        assert_eq!(
            closed_page.size.width - open_page.size.width,
            gpui::px(width),
            "the page only grows by the column: closing removes the column and nothing else"
        );
        restore_state(state);
    }


    /// The toolbar address edits through the Field overlay, which carries
    /// its rows over the page (canvas board "Field", 2026-10-11): no list
    /// draws in the window.
    #[gpui::test]
    fn omnibox_edits_through_the_field_overlay(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-omni-anchor");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_tab_new("https://www.wikipedia.org/", window, cx))
        })
        .expect("the tab opens");
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.focus_browse_address_with("wiki", window, cx))
        });
        draw_shell(&mut *vc, &baaz);
        assert!(vc.debug_bounds("browse-omni-list").is_none(), "the Field overlay, not an in-window list");
        assert!(vc.update(|_, cx| baaz.read(cx).field_overlay_wanted(cx)), "the Field overlay is wanted");
        restore_state(state);
    }

    /// With the sidebar hidden there is no overlay window: the Field is the
    /// same library panel in the main window, 640 wide, centred, its top at
    /// 18 % of the window (docs/83 §7) — never a full-width list.
    #[gpui::test]
    fn the_field_with_the_sidebar_hidden_is_the_centred_panel(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("browse-field-in-window");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        // A real window root (not `draw_shell`'s free element): the panel
        // rides a deferred draw, which needs the window's own frame.
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_tab_new("https://www.wikipedia.org/", window, cx))
        })
        .expect("the tab opens");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                if crate::layout::Layout::get(cx).sidebar_open {
                    h.toggle_sidebar(cx);
                }
            })
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.focus_browse_address_with("wiki", window, cx))
        });
        vc.simulate_resize(gpui::size(gpui::px(1440.), gpui::px(900.)));
        vc.run_until_parked();
        assert!(!vc.update(|_, cx| baaz.read(cx).field_overlay_wanted(cx)), "no overlay window with the sidebar hidden");
        let panel = vc.debug_bounds("field-panel").expect("the Field panel draws in the window");
        let win = vc.update(|window, _| window.viewport_size());
        let width = aui::overlay::palette_width(win.width);
        assert!((panel.size.width - width).abs() < gpui::px(1.0), "the panel is {:?} wide, not {width:?}", panel.size.width);
        assert!((panel.origin.x - (win.width - width) / 2.0).abs() < gpui::px(1.0), "centred: left {:?}", panel.origin.x);
        assert!((panel.origin.y - aui::overlay::palette_top(win.height)).abs() < gpui::px(1.0), "top at 18 %: {:?}", panel.origin.y);
        assert!(vc.debug_bounds("browse-omni-list").is_none(), "no full-width list under the toolbar");
        restore_state(state);
    }

    /// The Skills page's own header ("Skills · project · Search · Add
    /// skill") sits below the centre header's line, inset as the Settings
    /// column's heading is: once it drew dimmed and half under the
    /// window's top bar (2026-10-30).
    #[gpui::test]
    fn the_skills_header_clears_the_top_bar_and_its_fade(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("skills-header-top");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
                h.step_skills("page", window, cx).expect("the Skills page opens");
            })
        });
        draw_shell(&mut *vc, &baaz);
        let header = vc.debug_bounds("skills-header").expect("the Skills header draws");
        let bar = vc.update(|_, cx| {
            use aui_tokens::ActiveAui as _;
            f32::from(cx.aui().metrics.bar)
        });
        assert!(
            f32::from(header.origin.y) >= bar - 0.5,
            "the header starts below the centre header and its line ({bar}): {header:?}"
        );
        assert_eq!(
            crate::skills_page::SKILLS_HEADER_TOP,
            crate::settings::SETTINGS_COLUMN_TOP,
            "the header's inset is the Settings column's"
        );
        restore_state(state);
    }

    /// A corner stack over a centre with no composer wraps the centre's
    /// body. The wrapper was a block `div`, and the Skills page's root is
    /// `flex_1` — it took no height there, so the page drew a blank body
    /// (QA 2026-10-30). A toast holds the stack up.
    #[gpui::test]
    fn the_skills_page_fills_the_centre_under_a_corner_toast(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("skills-corner-toast");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
                h.step_skills("page", window, cx).expect("the Skills page opens");
            })
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            h.overlays.update(cx, |o, cx| {
                o.toast("Saved", "A toast in the corner stack.", cx);
            });
        }));
        draw_shell(&mut *vc, &baaz);
        assert!(vc.update(|_, cx| baaz.update(cx, |h, cx| h.render_corner_stack(cx).is_some())), "the corner stack shows");
        let list = vc.debug_bounds("skills-header").expect("the Skills header draws");
        assert!(f32::from(list.size.height) > 0.0, "the header has height: {list:?}");
        let page = vc.debug_bounds("skills-page").expect("the Skills page draws");
        assert!(f32::from(page.size.height) > 200.0, "the page fills the centre, not a zero-height strip: {page:?}");
        restore_state(state);
    }

    /// The Skills page keeps Settings' column: at a wide window its rows
    /// sit in a centred measure (Settings' column) instead of running edge to edge
    /// (owner 2026-10-31: "looks too wide and ugly"); at a narrow one the
    /// column is the page, never wider.
    #[gpui::test]
    fn the_skills_page_sits_in_the_settings_column(cx: &mut gpui::TestAppContext) {
        use gpui::IntoElement as _;
        assert_eq!(crate::skills_page::skills_column(false), (crate::settings::SETTINGS_COLUMN_MAX, 4.0));
        assert_eq!(crate::skills_page::skills_column(true).0, crate::settings::SETTINGS_COLUMN_MAX + 460.0);
        let state = hermetic_state("skills-column");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
                h.step_skills("page", window, cx).expect("the Skills page opens");
                // No skill picked: the list alone, in Settings' column.
                h.skills_page.update(cx, |page, _| page.selected = None);
            })
        });
        draw_shell(&mut *vc, &baaz);
        let page = vc.debug_bounds("skills-page").expect("the Skills page draws");
        let column = vc.debug_bounds("skills-column").expect("the Skills column draws");
        assert!(f32::from(page.size.width) > crate::settings::SETTINGS_COLUMN_MAX + 100.0, "a wide window: {page:?}");
        assert!(f32::from(column.size.width) <= crate::settings::SETTINGS_COLUMN_MAX + 0.5, "{column:?} in {page:?}");
        let left = f32::from(column.origin.x - page.origin.x);
        let right = f32::from((page.origin.x + page.size.width) - (column.origin.x + column.size.width));
        assert!((left - right).abs() < 1.0, "centred: {left} vs {right}");
        // Narrow: the column is the page, never past it.
        vc.simulate_resize(gpui::size(gpui::px(760.), gpui::px(700.)));
        vc.draw(gpui::point(gpui::px(0.), gpui::px(0.)), gpui::size(gpui::px(760.), gpui::px(700.)), |_, _| {
            baaz.clone().into_any_element()
        });
        let page = vc.debug_bounds("skills-page").expect("the Skills page draws narrow");
        let column = vc.debug_bounds("skills-column").expect("the Skills column draws narrow");
        assert!(column.size.width <= page.size.width + gpui::px(0.5), "{column:?} in {page:?}");
        assert!(column.origin.x >= page.origin.x - gpui::px(0.5));
        restore_state(state);
    }

    /// Settings › Providers' "New sessions start in" picker writes the same
    /// default the connect footer does: `provider.json`, and the window's
    /// own pick (owner 2026-10-31: the tracker sent them to a control
    /// Settings did not have).
    #[gpui::test]
    fn the_settings_default_provider_picker_writes_provider_json(cx: &mut gpui::TestAppContext) {
        use aui::screens::ConnectIntent;
        let state = hermetic_state("settings-default-provider");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
                h.open_settings_page("providers", cx);
            })
        });
        // The picker's menu is the page's own; a pick reaches the root as
        // an event, delivered when the update that emitted it ends.
        let page = vc.update(|_, cx| baaz.read(cx).settings_page.clone());
        let menu_open = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| page.read(cx).default_menu_open(cx));
        vc.update(|_, cx| page.update(cx, |page, cx| page.default_provider_intent(ConnectIntent::ToggleDefaultMenu, cx)));
        assert!(menu_open(vc), "the chip opens its menu");
        vc.update(|_, cx| page.update(cx, |page, cx| page.default_provider_intent(ConnectIntent::SetDefault(aui_icons::Provider::Codex), cx)));
        assert!(!menu_open(vc), "a pick closes it");
        assert_eq!(vc.update(|_, cx| baaz.read(cx).new_provider(cx).to_owned()), "codex");
        vc.update(|_, cx| page.update(cx, |page, cx| page.default_provider_intent(ConnectIntent::ToggleDefaultMenu, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("general", cx)));
        assert!(!menu_open(vc), "leaving the page closes the menu");
        let stored = std::fs::read_to_string(state.2.join("provider.json")).expect("provider.json written");
        assert!(stored.contains("\"codex\""), "{stored}");
        assert_eq!(crate::providers::read_last_provider(), Some(crate::providers::ProviderId::Codex));
        // The page draws with the group (the picker, or the
        // nothing-connected line when no provider is Connected here).
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("providers", cx)));
        draw_shell(&mut *vc, &baaz);
        restore_state(state);
    }

    /// The P7 import scan behind Settings › Providers' card line never runs
    /// on the UI thread: opening the page only starts it on the background
    /// executor, drawing the page reads the cache and never scans, and the
    /// plan lands when the executor runs (1.7 s cold over the owner's 3.6 GB
    /// `~/.claude`; the 2026-10-31 hang was on Settings).
    #[gpui::test]
    fn the_import_scan_never_runs_on_the_ui_thread(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("import-scan-thread");
        let owner = state.2.join("owner-home");
        crate::import::write_fixture_home(&owner, 30, 3, 0);
        let old_owner = std::env::var_os("BAAZ_IMPORT_OWNER_HOME");
        std::env::set_var("BAAZ_IMPORT_OWNER_HOME", &owner);
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.import.cache = None;
                h.import.scanning = false;
                h.open_settings_page("providers/claude-code", cx);
                assert!(h.import.scanning, "opening the page starts the scan");
                assert!(h.import.cache.is_none(), "the scan did not run inline on the UI thread");
            })
        });
        draw_shell(&mut *vc, &baaz);
        assert!(vc.update(|_, cx| baaz.read(cx).import.cache.is_none()), "drawing the page never scans");
        vc.run_until_parked();
        let offered = vc.update(|_, cx| {
            baaz.read(cx).import.cache.as_ref().map(|cache| cache.plan.sessions.len())
        });
        assert!(offered.is_some_and(|n| n > 0), "the background scan landed a plan: {offered:?}");
        match old_owner {
            Some(value) => std::env::set_var("BAAZ_IMPORT_OWNER_HOME", value),
            None => std::env::remove_var("BAAZ_IMPORT_OWNER_HOME"),
        }
        restore_state(state);
    }

    /// The Settings nav's "← Back" row sits below the sidebar column's
    /// chrome row and its line, the list inset down (once it drew half
    /// under the top bar's shade, owner screenshot 2026-10-31; the flat shell
    /// has no shade).
    #[gpui::test]
    fn the_settings_back_row_clears_the_top_bar(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("settings-back-top");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
                h.open_settings_page("general", cx);
            })
        });
        draw_shell(&mut *vc, &baaz);
        let back = vc.debug_bounds("settings-back").expect("the Back row draws");
        let row_h = vc.update(|_, cx| {
            use aui_tokens::ActiveAui as _;
            f32::from(cx.aui().metrics.header)
        });
        let clear = row_h + crate::settings::SETTINGS_NAV_TOP;
        assert!(
            f32::from(back.origin.y) >= clear - 0.5,
            "Back starts the list inset ({}) below the chrome row and its line ({row_h}): {back:?}",
            crate::settings::SETTINGS_NAV_TOP
        );
        restore_state(state);
    }

    /// B2a-H6: the Settings → Browser page draws — the dispatch lands on
    /// the new page with its Tabs, Search, Downloads and Chromium rows.
    #[gpui::test]
    fn settings_browser_page_draws_its_rows(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("settings-browser-page");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("browser", cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).settings_section(cx)),
            "browser",
            "settings:browser normalises onto the Browser page"
        );
        draw_shell(&mut *vc, &baaz);
        assert!(vc.debug_bounds("settings-search-engine").is_some(), "the engine choice draws");
        assert!(vc.debug_bounds("settings-downloads-change").is_some(), "the Change row draws");
        // The Chromium row shows only where the engine loads `chrome://`
        // pages (the `cef` backend) — anywhere else it hides.
        if cfg!(feature = "cef") {
            assert!(vc.debug_bounds("settings-browser-chromium").is_some(), "the Chromium row draws");
        } else {
            assert!(vc.debug_bounds("settings-browser-chromium").is_none(), "no Chromium row without the engine");
        }
        restore_state(state);
    }

    /// B2a-H6b: the Chromium row leaves Settings for Browse and shows
    /// `chrome://settings` in a person tab — selecting the existing tab
    /// when one already shows it instead of opening another.
    #[gpui::test]
    fn chromium_row_opens_its_settings_tab(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("chromium-row-tab");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.args.login = crate::LoginSample::SignedIn;
                h.apply_login_sample(window, cx);
            })
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("browser", cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.open_chromium_settings(window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(!harness.settings_open(cx), "the row leaves Settings");
            assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "the row lands in Browse");
            let id = harness.browser.read(cx).browse_centre_shown(cx).expect("a tab shows");
            let url = harness.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_default();
            assert_eq!(url, "chrome://settings", "the new tab loads Chromium's settings");
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.open_chromium_settings(window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let space = harness.browser.read(cx).browse_space();
            let settings_tabs: Vec<crate::browser::TabId> = harness
                .browser
                .read(cx)
                .tabs
                .person_tabs(space.as_deref())
                .iter()
                .filter(|tab| tab.url == crate::settings::CHROMIUM_SETTINGS_URL)
                .map(|tab| tab.id)
                .collect();
            assert_eq!(settings_tabs.len(), 1, "reopening selects the existing tab instead of opening another");
            assert_eq!(
                harness.browser.read(cx).browse_centre_shown(cx),
                Some(settings_tabs[0]),
                "the existing tab shows"
            );
        });
        restore_state(state);
    }

    /// B2a-H4d(6): blur only ends editing — never moves focus. With the
    /// address open, moving focus elsewhere (which blurs the input) drops
    /// the field and leaves focus where the user put it, instead of
    /// handing it back to the page.
    #[gpui::test]
    fn blur_ends_editing_without_moving_focus(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = hermetic_state("blur-no-steal");
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.ensure_browse_tab(window, cx);
                h.browse_centre_page(window, cx).expect("the centre builds its page");
                h.focus_browse_address_with("https://example.com", window, cx);
                // The page held the keyboard: a restore would hand focus
                // back to it — blur must not.
                h.browser.update(cx, |browser, _| browser.browse_address_page_focus = true);
            })
        });
        // A blur only ends the edit while the window is key (a blur from
        // the keyboard leaving for one of Baaz's own page windows does not).
        vc.update(|window, _| window.activate_window());
        vc.run_until_parked();
        // The input's own blur — exactly what a click on something
        // focusable delivers — through the real subscription arm.
        let held = vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let input = h.browser.read(cx).browse_address.clone().expect("the field is open");
                let held = window.focused(cx);
                input.update(cx, |_, cx| cx.emit(gpui_kit::base::input::InputEvent::Blur));
                held
            })
        });
        vc.run_until_parked();
        vc.update(|window, cx| {
            let h = baaz.read(cx);
            assert!(h.browser.read(cx).browse_address.is_none(), "blur ends the edit");
            assert_eq!(
                window.focused(cx),
                held,
                "blur never moves focus: whatever held it keeps it"
            );
        });
        restore_state(state);
    }

    /// B2a-H4d(7): selecting a sidebar tab moves the selection first, then
    /// leaves the address — so focus lands on the newly shown page (or the
    /// root), never the old page.
    #[gpui::test]
    fn select_leaves_the_address_onto_the_new_page(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = hermetic_state("select-new-page");
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(crate::layout::Mode::Browse, window, cx)));
        let (first, second) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.ensure_tabs_loaded(cx);
                let space = h.browser.read(cx).browse_space();
                let first = h.browser.update(cx, |browser, _| browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://first.example/", None));
                let second = h.browser.update(cx, |browser, _| browser.tabs.open(space, crate::browser::Owner::Person, "https://second.example/", None));
                h.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                (first, second)
            })
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.select_browse_sidebar_tab(first, window, cx);
                h.focus_browse_address_with("https://first.example/", window, cx);
                // The old page held the keyboard: leaving restores to a
                // page — which must be the newly shown one.
                h.browser.update(cx, |browser, _| browser.browse_address_page_focus = true);
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.select_browse_sidebar_tab(second, window, cx)));
        vc.update(|window, cx| {
            let h = baaz.read(cx);
            assert_eq!(h.browser.read(cx).browse_centre_shown(cx), Some(second), "the selection moved");
            assert!(h.browser.read(cx).browse_address.is_none(), "selecting leaves the address");
            let new_page = h.browse_centre_state(cx).expect("the new page exists");
            assert_eq!(
                window.focused(cx),
                Some(new_page.read(cx).focus_handle().clone()),
                "focus lands on the newly shown page, never the old one"
            );
            assert_ne!(first, second, "the test selected a different tab");
        });
        restore_state(state);
    }


    /// B7fix2: a rename inside the debounce window survives on disk.
    /// Newest wins: the debounced write serialises the CURRENT overrides
    /// at fire time, and the synchronous rename cancels the armed
    /// generation — so no stale snapshot ever lands over it.
    #[gpui::test]
    fn rename_within_the_debounce_window_survives_on_disk(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("rename-beats-debounce");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        // Arm the debounced pane save, then rename inside its window: the
        // rename writes synchronously and supersedes the armed write.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Changes, cx)));
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.set_override("s-1", |meta| meta.name = Some("Renamed".to_owned()), cx)
            })
        });
        // Past the store's 750 ms delay (real time: the store's writer is
        // a thread, not the test executor), then whatever is still queued
        // lands.
        std::thread::sleep(std::time::Duration::from_millis(850));
        crate::sessions::flush();
        for _ in 0..10 {
            vc.run_until_parked();
        }
        let disk = crate::sessions::read();
        assert_eq!(
            disk.get("s-1").and_then(|meta| meta.name.clone()),
            Some("Renamed".to_owned()),
            "the rename inside the debounce window survives on disk"
        );
        assert!(
            disk.get("s-1").and_then(|meta| meta.right.clone()).is_some_and(|right| right.open),
            "the debounced pane state still lands alongside the rename"
        );
        restore_state(state);
    }

    /// B7fix2: quitting flushes the pending pane save. The toggle arms the
    /// debounced write and stores nothing synchronously; the quit and
    /// window-close paths land it through `flush_stores`, so the pane
    /// survives a Cmd-Q inside the 750 ms window.
    #[gpui::test]
    fn quit_flushes_the_pending_pane_save(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("quit-flush");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Changes, cx)));
        assert!(
            !crate::sessions::stored(),
            "the toggle itself still writes nothing synchronously"
        );
        // What `QuitApp`, the red-dot close and the app-quit hook call.
        vc.update(|_, cx| baaz.update(cx, |h, _| h.flush_stores()));
        assert_eq!(
            crate::sessions::read().get("s-1").and_then(|meta| meta.right.clone()),
            Some(crate::sessions::RightState {
                open: true,
                kind: crate::layout::RightKind::Changes,
                files_preview: None,
                files_selected: None,
                files_expanded: Vec::new(),
                browser_url: None,
                lower: None,
                split: None,
            }),
            "quitting lands the pending pane state on disk"
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
                Harness::new(args, crate::runtime::CaptureToken::default(), window, cx)
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
                (matches!(h.auth, super::Auth::SignedIn(_)), h.active.is_none(), h.current_project(cx).is_some(), h.window_title(cx))
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
            // The shell's lifecycle ran against the live state: the title is
            // only written by the window's own sync (`sync_window_title`), so
            // a matching title proves the boot above settled.
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

    /// A 2,000-char first message titles the local row — and the header
    /// title reads the same row — with at most one 80-char line each. The
    /// first-send insert site clamps the prompt on the way in, and the
    /// header clamps whatever the row carries.
    #[gpui::test]
    fn a_two_thousand_char_first_message_yields_short_row_and_header_titles(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("long-title");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx)
        });
        let long = "w".repeat(2000);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| {
                    list.sessions.push(crate::sidebar::local_started_row(
                        "s-long",
                        crate::sidebar::one_line(&long),
                        None,
                        None,
                        crate::clock::now_local(),
                    ));
                });
                h.session_list.update(cx, |list, _| list.sessions_loaded = true);
                h.session_list.update(cx, |list, _| list.index_loaded = true);
                h.invalidate_list(cx);
            })
        });
        let (row_len, head_len) = vc.update(|_, cx| {
            let h = baaz.read(cx);
            let row = h.session_list.read(cx).sessions.iter().find(|e| e.id == "s-long").expect("the row");
            let head = h.collapsed_head_label("s-long", cx).expect("the header title");
            (row.label.chars().count(), head.chars().count())
        });
        assert!(row_len <= 80, "row title is {row_len} chars");
        assert!(head_len <= 80, "header title is {head_len} chars");
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "no session is open");
        for kind in crate::layout::RightKind::ALL {
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(kind, cx)));
            assert!(
                vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
                "{kind:?} opens the pane"
            );
            assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind), Some(kind));
            // The same kind again closes the pane, keeping the kind so a
            // later reopen restores it.
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.show_right(kind, cx)));
            assert!(
                !vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
                "{kind:?} again closes the pane"
            );
            assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind), Some(kind));
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "show_right never opens a session");
        }
        // A different kind while open switches without closing.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| h.show_right(crate::layout::RightKind::Changes, cx))
        });
        assert!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_open));
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind),
            Some(crate::layout::RightKind::Changes)
        );
        restore_state(state);
    }

    /// B8: `select_right` opens the pane on each of the three kinds and
    /// never closes it — not even when asked twice for the kind already
    /// showing (what a header tab click does), and never opening a session.
    #[gpui::test]
    fn select_right_opens_each_kind_and_never_closes(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("select");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "no session is open");
        for kind in crate::layout::RightKind::ALL {
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.select_right(kind, cx)));
            assert!(
                vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
                "{kind:?} opens the pane"
            );
            assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind), Some(kind));
            // The same kind again keeps the pane open on that kind.
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.select_right(kind, cx)));
            assert!(
                vc.update(|_, cx| crate::layout::Layout::get(cx).right_open),
                "{kind:?} again must not close the pane"
            );
            assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind), Some(kind));
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "select_right never opens a session");
        }
        restore_state(state);
    }

    /// docs/83 §6: the tab row carries Changes · Files · Page · Terminal
    /// (docs/82 8.1), each id
    /// parsing back to its kind and sitting at its strip index, the active
    /// one lit; Tasks joins them while it shows or the session has tasks
    /// (docs/82 6.5); a previewed file adds one closable tab that is lit in
    /// Files' place.
    #[test]
    fn the_tab_row_maps_to_the_kinds_and_the_open_file() {
        use crate::layout::RightKind;
        let tabs = crate::right::pane_tab_list(RightKind::Files, None, false);
        assert_eq!(tabs.len(), 4, "Changes, Files, Page, Terminal — no more without an open file or tasks");
        let with_tasks = crate::right::pane_tab_list(RightKind::Tasks, None, true);
        for (index, tab) in with_tasks.iter().enumerate() {
            let kind = RightKind::parse(&tab.id).unwrap_or_else(|| panic!("tab {:?} parses back to no kind", tab.id));
            assert_eq!(crate::right::screen_index(kind), index, "tab {:?} sits at its own index", tab.id);
            assert_eq!(kind.label(), tab.label.as_ref(), "tab {:?} is labelled as its kind", tab.id);
            assert_eq!(tab.active, kind == RightKind::Tasks);
        }
        assert_eq!(with_tasks.iter().map(|t| t.label.as_ref()).collect::<Vec<_>>(), ["Changes", "Files", "Page", "Terminal", "Tasks"]);
        assert_eq!(tabs.iter().map(|t| t.label.as_ref()).collect::<Vec<_>>(), ["Changes", "Files", "Page", "Terminal"]);
        assert!(!with_tasks.iter().any(|tab| tab.closable), "the views' tabs are fixed");

        let tabs = crate::right::pane_tab_list(RightKind::Files, Some("right.rs".into()), false);
        assert_eq!(tabs.len(), 5);
        assert_eq!((tabs[4].id.as_ref(), tabs[4].label.as_ref(), tabs[4].active, tabs[4].closable), (crate::right::FILE_TAB, "right.rs", true, true));
        assert!(!tabs[1].active, "Files is not lit while its file shows");
        let tabs = crate::right::pane_tab_list(RightKind::Changes, Some("right.rs".into()), false);
        assert!(tabs[0].active && !tabs[4].active, "on Changes the file's tab waits unlit");
    }

    /// Maximise flips between the widest pane and the default width.
    #[test]
    fn maximise_toggles_between_the_widest_and_the_default_width() {
        assert!(crate::right::right_maximised(aui::shell::RIGHT_MAX_WIDTH));
        assert!(!crate::right::right_maximised(aui::shell::RIGHT_WIDTH));
        assert!(!crate::right::right_maximised(aui::shell::RIGHT_MAX_WIDTH - 1.0));
    }

    /// Z2: the right pane belongs to each session and a switch restores it
    /// without animating. A shows the legacy Diff kind (saved and restored
    /// as Changes) while B shows Browser; every switch restores the shown
    /// session's pane (B first shows it closed); a restart restores from
    /// the store; the restore arms the pane's `snap` for exactly one frame while
    /// user toggles never arm it; a new session starts closed; Files
    /// previews are per session too.
    #[gpui::test]
    fn the_right_pane_is_per_session_and_restores_without_animating(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use gpui::prelude::*;
        let state = hermetic_state("right-per-session");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
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
                    (crate::layout::Layout::get(cx).right_open, crate::layout::Layout::get(cx).right_kind, h.right_pane.read(cx).snap)
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
        show!(crate::layout::RightKind::Changes);
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Changes), false));
        // Switching to B (no stored state) closes the pane, snapped — and
        // one frame consumes the snap.
        open_session!("sess-b");
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Changes), true));
        draw!();
        assert!(!vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).snap), "one frame consumes the snap");
        // Browser in B; a user change still does not snap.
        show!(crate::layout::RightKind::Browser);
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Browser), false));
        // Back to A: Diff again — open throughout, so no snap. Back to B:
        // Browser again, no snap either.
        open_session!("sess-a");
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Changes), false));
        open_session!("sess-b");
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Browser), false));
        draw!();
        // Closed to open snaps too: close in B, then return to A.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Browser), false));
        open_session!("sess-a");
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Changes), true));
        draw!();
        assert!(!vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).snap), "one frame consumes the snap");
        // A user toggle after the snap is consumed never re-arms it.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(!vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).snap), "a user toggle must still animate");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert_eq!(pane!(), (true, Some(crate::layout::RightKind::Changes), false));
        // A restore arms the snap; a user toggle BEFORE any frame consumes it
        // must still animate (review finding): the toggle disarms it.
        open_session!("sess-b");
        assert_eq!(pane!(), (false, Some(crate::layout::RightKind::Browser), true));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        assert!(!vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).snap), "a toggle before the frame still animates");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.toggle_right(cx)));
        open_session!("sess-a");
        draw!();
        // Restart: the store carries A home as Changes (the save
        // normalises the legacy Diff kind onto the merged view).
        let disk = crate::sessions::read();
        assert_eq!(
            disk.get("sess-a").and_then(|meta| meta.right.clone()),
            Some(crate::sessions::RightState {
                open: true,
                kind: crate::layout::RightKind::Changes,
                files_preview: None,
                files_selected: None,
                files_expanded: Vec::new(),
                browser_url: None,
                lower: None,
                split: None,
            })
        );
        let baaz2 = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|window, cx| baaz2.update(cx, |h, cx| h.resume("sess-a".to_owned(), window, cx)));
        assert!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_open), "A restores Changes after restart");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind),
            Some(crate::layout::RightKind::Changes)
        );
        // A brand-new session starts closed.
        vc.update(|window, cx| baaz2.update(cx, |h, cx| h.resume("sess-c".to_owned(), window, cx)));
        assert!(!vc.update(|_, cx| crate::layout::Layout::get(cx).right_open), "a new session starts closed");
        // Files previews are per session: A previews Cargo.toml, B nothing.
        open_session!("sess-a");
        show!(crate::layout::RightKind::Files);
        let root =
            vc.update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root).expect("a project"));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.with_right_pane(cx, |pane, cx| pane.begin_file_preview_for(&root, "Cargo.toml", cx))));
        macro_rules! previewed {
            () => {
                vc.update(|_, cx| {
                    baaz.read(cx).right_pane.read(cx).cache.preview_for(&root).map(|preview| preview.path.clone())
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let dir = std::env::temp_dir().join(format!("baaz-right-pure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("purity probe dir");
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let project = Some((dir.clone(), "pure".to_string()));
        crate::right::reset_io_count();
        for kind in crate::layout::RightKind::ALL {
            for _ in 0..2 {
                vc.update(|window, cx| {
                    let pane = baaz.read(cx).right_pane.clone();
                    let terminal = baaz.read(cx).terminal_dock.clone();
                    pane.update(cx, |pane, cx| {
                        let _ = crate::right::render(
                            kind,
                            &pane.cache,
                            project.clone(),
                            &[],
                            crate::right::BrowserPane { state: None, window: &mut *window, offers: Default::default() },
                            &terminal,
                            true,
                            400.0,
                            "Muse",
                            false,
                            cx,
                        );
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.run_until_parked();
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        let files = vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).cache.files_for(&root));
        assert!(files.is_some(), "the background re-read lands the file listing in the cache");
        crate::right::reset_io_count();
        // Outside any `Harness` update, exactly like the pane's own action
        // dispatch: nesting an entity update inside one panics.
        vc.update(|_, cx| {
            baaz.update(cx, |this, cx| this.refresh_right_now(cx));
        });
        vc.run_until_parked();
        assert!(
            crate::right::io_count() > 0,
            "Refresh re-reads the filesystem instead of toasting that it is not wired"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).cache.files_for(&root)).is_some(),
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.run_until_parked();
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        std::fs::write(root.join("note.txt"), "version one\n").unwrap();
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.with_right_pane(cx, |pane, cx| pane.begin_file_preview_for(&root, "note.txt", cx))));
        vc.run_until_parked();
        let shows = |vc: &mut gpui::VisualTestContext, baaz: &gpui::Entity<Harness>| {
            vc.update(|_, cx| {
                baaz.read(cx)
                    .right_pane
                    .read(cx)
                    .cache
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

    /// B9: a link's URL opens the pane on Browser and navigates the fake
    /// backend at once — no frame, no window needed for the scripted page.
    #[gpui::test]
    fn url_links_open_the_browser_pane_and_navigate(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("url-link-browser");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        let view = vc.update(|_, cx| baaz.read(cx).active.clone().expect("a session"));
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.on_session_event(
                    view.read(cx).runtime().clone(),
                    &crate::session::SessionEvent::OpenUrl {
                        url: "https://example.com/page".to_owned(),
                        external: false,
                    },
                    cx,
                );
            })
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(crate::layout::Layout::get(cx).right_open, "the link opens the pane");
            assert_eq!(
                crate::layout::right_kind(crate::layout::Layout::get(cx)),
                crate::layout::RightKind::Browser,
                "the link opens the pane on Browser"
            );
            let url =
                harness.browser.read(cx).session_page("s-1").map(|state| state.read(cx).url().to_string());
            assert_eq!(url.as_deref(), Some("https://example.com/page"), "the fake page navigates");
            assert!(
                !harness.browser.read(cx).browser_url_focus_armed,
                "a link never earns URL-field focus"
            );
        });
        restore_state(state);
    }

    /// B9: a link to a folder selects it with every ancestor expanded and
    /// no preview — through the event's own background existence check.
    #[gpui::test]
    fn folder_links_reveal_with_expanded_ancestors(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("folder-link-reveal");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        std::fs::create_dir_all(root.join("crates/baaz/src")).unwrap();
        let view = vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let host = crate::session::SessionHost {
                    provider_id: "echo".to_owned(),
                    workspace: root.to_string_lossy().into_owned(),
                    overlays: harness.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view = cx.new(|cx| {
                    crate::session::SessionView::new("s-1".to_owned(), host, window, cx)
                });
                harness.active = Some(view.clone());
                view
            })
        });
        let abs = root.join("crates/baaz/src");
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.on_session_event(
                    view.read(cx).runtime().clone(),
                    &crate::session::SessionEvent::RevealPath { abs: abs.clone(), highlight: None },
                    cx,
                );
            })
        });
        vc.run_until_parked();
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(crate::layout::Layout::get(cx).right_open, "the link opens the pane");
            assert_eq!(
                crate::layout::right_kind(crate::layout::Layout::get(cx)),
                crate::layout::RightKind::Files,
                "a folder link opens the pane on Files"
            );
            let open = harness.right_pane.read(cx).cache.expanded_for(&root);
            assert!(
                open.contains("crates") && open.contains("crates/baaz") && open.contains("crates/baaz/src"),
                "every ancestor stands open, got: {open:?}"
            );
            assert_eq!(
                harness.right_pane.read(cx).cache.selected_for(&root).as_deref(),
                Some("crates/baaz/src"),
                "the folder is selected"
            );
            assert!(
                harness.right_pane.read(cx).cache.preview_for(&root).is_none(),
                "a folder reveal previews nothing"
            );
        });
        restore_state(state);
    }

    /// B9: a link to a file previews it banded on the linked lines.
    #[gpui::test]
    fn file_links_preview_with_their_lines(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("file-link-preview");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        let view = vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let host = crate::session::SessionHost {
                    provider_id: "echo".to_owned(),
                    workspace: root.to_string_lossy().into_owned(),
                    overlays: harness.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view = cx.new(|cx| {
                    crate::session::SessionView::new("s-1".to_owned(), host, window, cx)
                });
                harness.active = Some(view.clone());
                view
            })
        });
        let abs = root.join("src/main.rs");
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.on_session_event(
                    view.read(cx).runtime().clone(),
                    &crate::session::SessionEvent::RevealPath {
                        abs: abs.clone(),
                        highlight: Some(2..3),
                    },
                    cx,
                );
            })
        });
        vc.run_until_parked();
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(
                crate::layout::right_kind(crate::layout::Layout::get(cx)),
                crate::layout::RightKind::Files,
                "a file link opens the pane on Files"
            );
            let preview = harness.right_pane.read(cx).cache.preview_for(&root).expect("a preview");
            assert_eq!(preview.path, "src/main.rs");
            assert_eq!(preview.highlight, Some(2..3), "the preview bands the linked lines");
            assert!(
                harness.right_pane.read(cx).cache.expanded_for(&root).contains("src"),
                "the file's parent stands open"
            );
        });
        restore_state(state);
    }

    /// Owner 2026-10-21: an HTML file linked from the transcript opened
    /// as code in Files. It renders in the Page now.
    #[gpui::test]
    fn an_html_link_opens_in_the_page(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("html-in-page");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        std::fs::create_dir_all(root.join("site")).unwrap();
        std::fs::write(root.join("site/kitchen sink.html"), "<h1>hi</h1>").unwrap();
        let view = vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let host = crate::session::SessionHost {
                    provider_id: "echo".to_owned(),
                    workspace: root.to_string_lossy().into_owned(),
                    overlays: harness.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view = cx.new(|cx| crate::session::SessionView::new("s-1".to_owned(), host, window, cx));
                harness.active = Some(view.clone());
                view
            })
        });
        let page_shows = |vc: &mut gpui::VisualTestContext, url: &str| {
            vc.update(|_, cx| {
                let harness = baaz.read(cx);
                harness.browser.read(cx).browser_pending_url.as_deref() == Some(url)
                    || harness.browser.read(cx).states.values().any(|state| state.read(cx).url().as_ref() == url)
            })
        };
        let abs = root.join("site/kitchen sink.html");
        let url = crate::right::file_url(&abs);
        assert!(url.ends_with("/site/kitchen%20sink.html"), "{url}");
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.on_session_event(view.read(cx).runtime().clone(), &crate::session::SessionEvent::RevealPath { abs: abs.clone(), highlight: None }, cx);
            })
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            assert_eq!(crate::layout::right_kind(crate::layout::Layout::get(cx)), crate::layout::RightKind::Browser, "an HTML link opens the Page");
        });
        assert!(page_shows(vc, &url), "the Page navigates to the file");
        assert!(vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).cache.preview_for(&root).is_none()), "no code preview opened");
        restore_state(state);
    }

    /// Owner 2026-10-21: clicking kitchen-sink.html in Files opened its
    /// code. A Files click on HTML opens the Page; "View source" on a
    /// Markdown file previews its text, not the rendering.
    #[gpui::test]
    fn a_files_click_on_html_opens_the_page_and_view_source_shows_text(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("files-html");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project(cx).map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        std::fs::write(root.join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(root.join("notes.md"), "# Notes\n").unwrap();
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.select_right(crate::layout::RightKind::Files, cx)));
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.with_right_pane(cx, |pane, cx| pane.open_file_entry(&root, "index.html", cx))));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(crate::layout::right_kind(crate::layout::Layout::get(cx)), crate::layout::RightKind::Browser, "a Files click on HTML opens the Page");
            assert!(harness.right_pane.read(cx).cache.preview_for(&root).is_none(), "no code preview");
        });
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.with_right_pane(cx, |pane, cx| pane.begin_file_source_for(&root, "notes.md", cx))));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let preview = baaz.read(cx).right_pane.read(cx).cache.preview_for(&root).expect("a source preview");
            assert!(matches!(preview.content, crate::right::PreviewContent::Text { .. }), "View source shows text, drew {:?}", preview.content);
        });
        // Without "View source" the same file renders.
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.with_right_pane(cx, |pane, cx| pane.open_file_entry(&root, "notes.md", cx))));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let preview = baaz.read(cx).right_pane.read(cx).cache.preview_for(&root).expect("a preview");
            assert!(matches!(preview.content, crate::right::PreviewContent::Markdown { .. }), "a plain open renders, drew {:?}", preview.content);
        });
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.begin_resize(100.0, cx);
                assert!(h.sidebar_resizing(cx));
                h.begin_right_resize(100.0, cx);
                assert!(!h.right_pane.read(cx).resize.active, "a right press mid-sidebar-drag is a no-op");
                h.end_resize(cx);
                assert!(!h.sidebar_resizing(cx));
                h.begin_right_resize(100.0, cx);
                assert!(h.right_pane.read(cx).resize.active);
                h.begin_resize(100.0, cx);
                assert!(!h.sidebar_resizing(cx), "a sidebar press mid-right-drag is a no-op");
                h.end_right_resize(cx);
                assert!(!h.right_pane.read(cx).resize.active);
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
            cx.new(|cx| Harness::new(test_args(dir), crate::runtime::CaptureToken::default(), window, cx))
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
                harness.settings_page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Set("cmd-t".into()), cx));
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
            vc.update(|_, cx| baaz.read(cx).settings_page.read(cx).shortcut_errors.is_empty()),
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
                harness.settings_page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Set("cmd-q".into()), cx));
            });
        });
        let error = vc.update(|_, cx| baaz.read(cx).settings_page.read(cx).shortcut_errors.get(id.as_ref()).cloned());
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
                harness.settings_page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Record, cx));
            });
        });
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).settings_page.read(cx).recording.clone()),
            Some(id.to_string()),
            "Record arms the row"
        );
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.settings_page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Cancel, cx));
            });
        });
        assert!(
            vc.update(|_, cx| baaz.read(cx).settings_page.read(cx).recording.clone()).is_none(),
            "Cancel disarms it"
        );
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.settings_page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Set("cmd-t".into()), cx));
            });
        });
        let rebound = crate::keymap::effective_binding("NewSession", Some("AuiRoot")).expect("rebound");
        assert_eq!(rebound.keystroke, "cmd-t");
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.settings_page.update(cx, |page, cx| page.handle_shortcut(&id, &aui::overlay::ShortcutEdit::Clear, cx));
            });
        });
        let file = std::fs::read_to_string(state.2.join("keymap.json")).unwrap_or_default();
        assert!(!file.contains("cmd-t"), "Clear drops the rebind from the file: {file}");
        let current = crate::keymap::effective_binding("NewSession", Some("AuiRoot")).expect("still bound");
        assert_eq!(current.keystroke, "cmd-n", "Clear restores the default");
        restore_shortcut_state(state);
    }

    /// P4: a Muse row follows the hub's connection stream, open or not —
    /// a running turn arms it, a status broadcast's attention marks it
    /// needing approval, a failed settle records the failure, and the idle
    /// broadcast stands it down.
    #[gpui::test]
    fn the_hubs_connection_events_drive_muse_rows(cx: &mut gpui::TestAppContext) {
        use chrono::Local;
        let state = hermetic_state("hub-rows");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let row = |baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).session_list.read(cx).sessions.iter().find(|e| e.id == "s-bg").cloned().expect("the row"))
        };
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                let mut entry = crate::sidebar::local_started_row("s-bg", "Chart it".into(), None, None, Local::now());
                entry.running = false;
                entry.turns = 1;
                h.session_list.update(cx, |list, _| list.sessions.push(entry));
                h.on_hub_event(provider::ProviderEvent::TurnRunning { session_id: "s-bg".into(), turn_id: "t-1".into() }, cx);
            })
        });
        assert!(row(&baaz, vc).running, "a running turn arms the row");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.on_hub_event(
                    provider::ProviderEvent::StatusChanged {
                        session_id: "s-bg".into(),
                        running: true,
                        attention: vec![provider::facts::Attention::ApprovalPending],
                    },
                    cx,
                );
            })
        });
        assert!(row(&baaz, vc).needs_approval(), "the broadcast's attention reaches the row");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.on_hub_event(
                    provider::ProviderEvent::TurnSettled {
                        session_id: "s-bg".into(),
                        turn_id: "t-1".into(),
                        outcome: provider::facts::TurnOutcome::Failed {
                            message: Some("rate limited".into()),
                            signed_out: false,
                        },
                    },
                    cx,
                );
            })
        });
        assert_eq!(row(&baaz, vc).last_error.as_deref(), Some("rate limited"), "the settle records why it failed");
        // The idle broadcast that follows stands the row down.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.on_hub_event(
                    provider::ProviderEvent::StatusChanged { session_id: "s-bg".into(), running: false, attention: Vec::new() },
                    cx,
                );
            })
        });
        let idle = row(&baaz, vc);
        assert!(!idle.running && !idle.needs_approval(), "idle, nothing waiting");
        restore_state(state);
    }

    /// B5: a `session/list` reply that still says idle never stands a live
    /// turn down. The reply's wholesale replace clears the open row's
    /// running bit; the merge tail lays every live view back over its row,
    /// so the row keeps `Working` instead of flipping to `Settled` until
    /// the turn's next event.
    #[gpui::test]
    fn list_reply_after_a_live_turn_keeps_the_row_working(cx: &mut gpui::TestAppContext) {
        use chrono::Local;
        let state = hermetic_state("list-live-row");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        open_test_session(&mut *vc, &baaz, &state.2);
        fn idle_row(id: &str) -> crate::sidebar::SessionEntry {
            crate::sidebar::SessionEntry {
                id: id.to_owned(),
                label: "x".into(),
                updated: Local::now(),
                created: Local::now(),
                running: false,
                turns: 1,
                hidden: false,
                pinned: false,
                archived: false,
                description: String::new(),
                replayed: false,
                provider: crate::providers::ProviderId::Muse,
                named: false,
                needs_title: false,
                side_marker: false,
                title_pending: false,
                last_ask: None,
                branch: None,
                tasks_running: 0,
                pages: 0,
                pull_request: None,
                local: false,
                provisional: false,
                turns_unknown: false,
                workspace: None,
                project: None,
                project_name: None,
                attention: Vec::new(),
                approval_command: None,
                pending_question: None,
                turn_started: None,
                last_error: None,
                unread: false,
            }
        }
        // The live turn, the way `turn/started` leaves it: the view holds
        // the turn and the row reads running from it.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| list.sessions.push(idle_row("s-1")));
                let view = h.active.clone().expect("a session is open");
                view.update(cx, |view, cx| {
                    view.runtime().clone().update(cx, |rt, cx| rt.apply(crate::session::wire_note("turn/started", "s-1", serde_json::json!({"turnId": "t-1"})), cx));
                });
                h.sync_row_live("s-1", true, cx);
            })
        });
        assert!(
            vc.update(|_, cx| baaz.read(cx).session_list.read(cx).sessions.iter().any(|e| e.id == "s-1" && e.running)),
            "the started turn arms the row"
        );
        // The list reply still says idle: the wholesale replace stands the
        // row down, then the merge tail lays the live view back over it —
        // exactly the order `load_sessions` applies.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                let wire = vec![idle_row("s-1")];
                h.session_list.update(cx, |list, _| list.sessions = crate::sidebar::merge_session_list(wire, &list.sessions));
                h.merge_provider_rows(cx);
                h.sync_all_live_rows(cx);
            })
        });
        let (running, kind) = vc.update(|_, cx| {
            let h = baaz.read(cx);
            let entry = h.session_list.read(cx).sessions.iter().find(|e| e.id == "s-1").expect("the row survives the merge");
            let status = entry.row_status(Local::now());
            (entry.running, status.kind)
        });
        assert!(running, "the live view re-arms the row after the list reply");
        assert_eq!(kind, aui::nav::RowStatusKind::Working, "a busy view never reads Settled");
        restore_state(state);
    }

}
