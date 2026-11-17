//! Browse mode's state in one struct (task 4.5, audit R4 step one): what
//! was ~45 loose fields on `Harness` beside the old `BrowserRegistry`, now
//! owned as `Harness::browser`. The second step promotes it to an entity.

use std::collections::{HashMap, HashSet};

use aui_webview::WebviewState;
use gpui::{AppContext as _, Bounds, Entity, Pixels};

use super::{chrome, history, peek_split, BackForward, TabId, Tabs};
use super::favicons;

/// Browse mode's state, owned by [`Harness`](crate::app::Harness) as one
/// field (task 4.5): every webview and the tab model, the Field and find
/// inputs, the Browse sidebar's menus and drags, the overlay windows over the
/// native page, and the `browser.db` service.
pub(crate) struct BrowseState {
    /// One live state per open tab, keyed by tab. Hidden on switch, never
    /// destroyed — except by discard, which drops the state and keeps the
    /// tab's URL for a rebuild on show.
    pub states: HashMap<TabId, Entity<WebviewState>>,
    /// Favicon URL → local file ([`favicons`]).
    pub favicons: favicons::Favicons,
    /// Set at boot: scripted page (`--screenshot`, tests) vs WKWebView.
    pub fake: bool,
    /// An external file drag currently covers the window.
    pub drop_cover: bool,
    /// Sessions whose Screenshot intent still waits for bytes.
    pub shot_pending: HashSet<String>,
    /// The tab model: tabs per session and per project, persisted.
    pub tabs: Tabs,
    /// Whether [`Tabs`] has been loaded and migrated yet. Lazy: the
    /// registry is built before `sessions.json` is read, so the first
    /// browser use loads.
    pub tabs_loaded: bool,
    /// The tab the agent tools act on per session key, so discard never
    /// drops the agent's active tab and switches re-register it.
    pub agent_tabs: HashMap<String, TabId>,
    /// The strip tab holding a middle press: set on middle mouse down,
    /// compared on middle mouse up (only the same tab closes), cleared on
    /// every middle up. Never rendered — pure press/release pairing.
    /// The Browse centre page's rect, stored by `sync_browser` every
    /// frame: the mouse-down hand-back measures against this same rect
    /// (see `browse_centre_page_rect`), never a second computation.
    /// `None` outside Browse and before the first sync.
    pub centre_page_rect: Option<Bounds<Pixels>>,
    /// The loading line's rect while a page loads (v3): cut through the
    /// page as a hole so the line shows along the page column's top edge.
    pub progress_hole: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    /// The page the v3 holes were last cut through, so they are taken back
    /// when it stops being the centre.
    pub holes_on: Option<gpui::WeakEntity<WebviewState>>,
    /// One back/forward stack per tab (B2a-H4): the header toolbar tracks
    /// the URLs the sync reports, since the library exposes no back,
    /// forward or reload calls to drive.
    pub histories: HashMap<TabId, BackForward>,
    /// Every tab's find strip (B2a-H5): per tab, so switching hides the
    /// strip and returning restores the query.
    pub find_strips: chrome::FindStrips,
    /// Origins trusted "while visiting" (B2a-H5): in memory for this run
    /// only — persistence is B2b.
    pub visit_allows: chrome::VisitAllows,
    /// Each live tab's last seen origin: navigating to another origin (or
    /// closing) revokes that origin's "while visiting" grants.
    pub permission_origins: HashMap<TabId, String>,
    /// Which Browse chrome popover stands open (B2a-H5): downloads (the
    /// toolbar button exists only while downloads list), extensions, ⋯.
    /// They drop over the page; see [`BrowserVisibility`].
    pub downloads_open: bool,
    /// Where the sidebar's downloads button is (D4), in window
    /// coordinates: the popover opens under it.
    pub downloads_anchor: Option<gpui::Bounds<gpui::Pixels>>,
    /// The extensions popover stands open.
    pub extensions_open: bool,
    /// Whether the downloads popover has rendered since it opened: an
    /// emptied list clears the flag only after it rendered once, so a
    /// scripted download's first frame never closes its own capture.
    pub downloads_rendered: bool,
    /// The ⋯ page-actions menu stands open.
    pub more_open: bool,
    /// Scripted fake-backend events, queued by `--steps` verbs and consumed
    /// when the next scripted page builds (captures only; production never
    /// queues). The events drain through the real poll path, so captures
    /// show the same strips a live page would.
    pub fake_scripts: Vec<chrome::FakePageScript>,
    /// When each live download started arriving, as ms since the epoch
    /// ([`tabs::now_ms`]): the popover's "6 s left" estimates read this.
    /// Stamped on first sight, pruned when the download ends.
    pub download_started: HashMap<(TabId, u32), u64>,
    /// The centre tab the last sync saw: a switch dismisses the chrome
    /// popovers (they point at the old tab's state) but never the find
    /// strip, which is per tab and restores on return.
    pub last_centre_shown: Option<TabId>,
    /// A centre switch waiting for find to follow it: (left, shown).
    pub find_resync: Option<(Option<TabId>, Option<TabId>)>,
    /// R4 `browser.db` as a service (task 4.5): history, page words and
    /// bookmarks, run on the background executor behind an in-memory front
    /// the UI reads ([`super::data::BrowserData`]).
    pub data: Entity<super::data::BrowserData>,
    /// Which page each person tab was recorded at, and its dwell.
    pub recorder: history::Recorder,
    /// Page-word reads in flight: request id → (tab, url, title).
    pub text_reads: HashMap<u64, (TabId, String, String)>,
    /// The next page-word request id (a range no other caller uses).
    pub next_text_read: u64,
    /// Ad-box scans in flight: request id → (tab, url) (`adblock` cosmetics).
    pub cosmetic_reads: HashMap<u64, (TabId, String)>,
    /// Stylesheet installs in flight; their answers are only drained.
    pub cosmetic_injects: std::collections::HashSet<u64>,
    /// Each tab's scans: the page they were for, how many ran, when the first.
    pub cosmetic_pass: HashMap<TabId, (String, u8, std::time::Instant)>,
    /// A wake-up is armed for the shown tab's dwell, at this page.
    pub dwell_armed: Option<(TabId, String)>,
    /// The Field's scope (R4 board 1c): `Some(folder)` while it shows only
    /// bookmarks, `None` inside means the top level.
    pub field_scope: Option<Option<i64>>,
    /// The Field's input wears the scoped placeholder.
    pub placeholder_scoped: bool,
    /// R4: the open Peek, if any.
    pub peek: Option<peek_split::Peek>,
    /// R4: the open Split, if any.
    pub split: Option<peek_split::SplitPair>,
    /// The left half's share of a Split (dragged by the divider).
    pub split_ratio: f32,
    /// The Split divider is being dragged.
    pub split_dragging: bool,
    /// A fresh Peek's page takes the keyboard on the next render.
    pub peek_focus_pending: bool,
    /// The Split half the person clicked into: shown on the next render.
    pub split_focus: Option<TabId>,
    /// Whether the Split partner held the keyboard last frame.
    pub partner_held: bool,
    /// Done closes tabs (docs/27 §4.3): sessions this run saw working, and
    /// when each task folder went done.
    pub(crate) task_seen_working: std::collections::HashSet<String>,
    pub(crate) task_done_at: std::collections::HashMap<String, u64>,
    /// Wakes the window when the next done task folder is due to close.
    pub(crate) task_done_timer: Option<gpui::Task<()>>,
    /// How many webviews the open actions built (B7fix2): the spy the
    /// render-must-not-construct test reads. Incremented wherever a
    /// webview is actually constructed, never when an existing one is
    /// reused — so one open action plus any number of frames stays one.
    pub(crate) browser_builds: u64,
    /// Whether the one idle-time Browser warmup already ran (B7fix2): set
    /// the first signed-in frame, so the boot frame never constructs even
    /// when a restore left the pane open on Browser.
    pub(crate) browser_idle_warmed: bool,
    /// The person's own Browser open still owes the URL field its focus
    /// (Z7a2). Armed by `show_right`/`toggle_right` when they land open on
    /// Browser, consumed once by the next frame's webview ensure — and only
    /// there, so activation, restore and boot never steal the keyboard into
    /// the URL field. Never persists.
    pub(crate) browser_url_focus_armed: bool,
    /// A link's URL waiting for the Browser pane's next render (B9): set
    /// by [`Harness::open_url_in_browser`](crate::browser) beside the
    /// non-toggling open, consumed once by `ensure_browser_person` where
    /// the window is at hand. The fake backend navigates without a window
    /// and never arms this. Never persists.
    pub(crate) browser_pending_url: Option<String>,
    /// The Browse address field's live input (B2a-H4): `Some` while the
    /// toolbar or the new-tab field is being edited, `None` at rest. One
    /// input for both fields — only one ever renders it per frame — so the
    /// omnibox suggestions read one value everywhere. Never persists.
    pub(crate) browse_address: Option<Entity<gpui_kit::base::input::InputState>>,
    /// The address input's event subscription (Enter commits, blur
    /// restores): held while [`Self::browse_address`] is `Some`, dropped
    /// with it. Never persists.
    pub(crate) browse_address_sub: Option<gpui::Subscription>,
    /// The omnibox suggestion list's highlighted row, across all sections
    /// in order (like the palette's `selected`). Meaningful only while
    /// [`Self::browse_address`] is `Some`. Never persists.
    pub(crate) browse_omnibox_selected: usize,
    /// The new tab's "Pick up" row the pointer is over (its row list's
    /// pointer report), so that row's text lights with its plain hover.
    pub(crate) browse_pickup_hover: Option<usize>,
    /// The new tab's baked ground ([`crate::newtab_ground`]): `None` until
    /// the first new tab asks for it and the background bake lands, then
    /// the file the page paints. `newtab_ground_asked` keeps it to one bake
    /// per launch.
    pub(crate) newtab_ground: Option<std::path::PathBuf>,
    /// The site-information panel (the address row's shield) stands open
    /// over the page ([`crate::overlay`]).
    pub(crate) site_info_open: bool,
    /// R4: the bookmark popover's bookmark, while it stands open.
    pub(crate) bookmark_edit: Option<crate::overlay::BookmarkEdit>,
    pub(crate) bookmark_edit_sub: Option<gpui::Subscription>,
    pub(crate) newtab_ground_asked: bool,
    /// Whether the highlight moved off the top Ask row (B2a-H4): Enter
    /// activates the highlighted row once moved, and commits the raw text
    /// otherwise — so plain Enter navigates instead of asking. Set by
    /// `move_omnibox`, cleared by typing and whenever editing ends. Never
    /// persists.
    pub(crate) browse_omnibox_moved: bool,
    /// The header back button's rect (B2a-H4): what the back-history menu
    /// seats at, reported the way `overflow_bounds` is. Never persists.
    pub(crate) back_bounds: Option<Bounds<Pixels>>,
    /// Whether the Browse centre's page held the keyboard on the last
    /// `sync_browser` (B2a-H4 7b): the rising edge ends address editing,
    /// since a click into a Chromium page moves no gpui focus and the input
    /// never blurs. Never persists.
    pub(crate) browse_last_holds: bool,
    /// Whether the Browse centre's page held the keyboard when the address
    /// field took focus (B2a-H4c): leaving the field hands focus back to
    /// the page then, else to the root — never a destroyed handle. Never
    /// persists.
    pub(crate) browse_address_page_focus: bool,
    /// The find strip's live input (B2a-H5): `Some` while the strip renders
    /// on the shown tab, `None` when it closes. One input for every tab —
    /// the render syncs its value to the shown tab's kept query, so
    /// switching tabs never shows another tab's text. Never persists.
    pub(crate) browse_find: Option<Entity<gpui_kit::base::input::InputState>>,
    /// The find input's event subscription (Enter steps, typing searches
    /// debounced): held while [`Self::browse_find`] is `Some`, dropped
    /// with it. Never persists.
    pub(crate) browse_find_sub: Option<gpui::Subscription>,
    /// The Ask question waiting for its session (B2a-H4d): `browse_ask`
    /// arms this and starts the session; `activate` writes it into the
    /// landed view's draft (appending on a new line over reused text) —
    /// never sent, never on the wrong session. Keyed by the session id
    /// `new_session_in` creates (or the draft id it reuses): `None` while
    /// an async open is in flight — never taken by an unrelated landing —
    /// and dropped on every early return. Taken on landing. Never
    /// persists.
    pub(crate) pending_ask_draft: Option<crate::app::PendingAsk>,
    /// The omnibox row Tab completed into the field (B2a-H4d): completing
    /// remembers the completed ROW's identity, not an index into a
    /// re-ranked list — Enter opens this row even after the ranking moved
    /// under it. Cleared whenever the edit ends or the text changes. Never
    /// persists.
    pub(crate) browse_completed: Option<crate::browser::OmniTarget>,
    /// The text Tab wrote for [`Self::browse_completed`]: any other text in
    /// the field means the person typed past it. Never persists.
    pub(crate) browse_completed_text: Option<String>,
    /// Outside URLs waiting for the shell (B2a-H7): `ingest_external_urls`
    /// parks them while the login stands open;
    /// `flush_pending_external_urls` opens them once signed in. Never
    /// persists.
    pub(crate) pending_external_urls: Vec<String>,
    /// The cached `https` default handler (B2a-H7b): `None` until the
    /// first read, then the lookup result (`None` inside when the lookup
    /// fails, e.g. off macOS). The Settings page renders this cache
    /// only — never a live LaunchServices lookup — and [`Self::on_frame`]
    /// refreshes it on focus-regain, when the Make-default re-read comes
    /// due, and on the first frame. Never persists.
    pub(crate) default_browser_cache: Option<Option<String>>,
    /// A Make-default click is waiting for macOS to confirm: the row
    /// shows "Waiting for macOS to confirm…" until the next re-read.
    /// Never persists.
    pub(crate) default_browser_confirm_pending: bool,
    /// When the post-Make-default re-read comes due (macOS confirms
    /// asynchronously — about a second after the click — plus again on
    /// the next focus-regain). Never persists.
    pub(crate) default_browser_recheck_at: Option<std::time::Instant>,
    /// The tab row a drag hovers over: the 2 px accent line's slot while
    /// a tab drag is in flight. Transient (cleared on drop), never keyed.
    pub(crate) browse_drop_target: Option<crate::browser::TabId>,
    /// The drop-at-end slot below the last tab row: the accent line's
    /// slot when a tab drag rests past every row (drops append at the
    /// end). Transient like the row target, never keyed.
    pub(crate) browse_drop_at_end: bool,
    /// The tab whose right-click menu stands open in the Browse sidebar
    /// (`None` while none does). Part of the sidebar key.
    pub(crate) tab_menu: Option<crate::browser::TabId>,
    /// The Browse sidebar's open right-click menu (tab, folder, task
    /// folder or space) at the pointer; drawn in the overlay window.
    pub(crate) browse_menu: Option<aui::screens::sidebar::BrowseMenu>,
    /// When the page menu last closed: the press on ⋯ that closes it (by
    /// taking the keyboard from its overlay window) must not reopen it on
    /// the same click's release.
    pub(crate) page_menu_closed: Option<std::time::Instant>,
    /// The Browse name field in flight (a folder or a space), edited in
    /// the shared `rename` textarea.
    pub(crate) browse_naming: Option<aui::screens::sidebar::BrowseNaming>,
    /// The overlay window over the native page (Browse menus), when open.
    pub(crate) overlay: Option<crate::overlay::Overlay>,
    /// An overlay being opened (its task is in flight), as (kind, bounds).
    pub(crate) overlay_pending: Option<(crate::overlay::OverlayKind, gpui::Bounds<gpui::Pixels>)>,
    /// The corner window (Browse's corner stack), beside the overlay.
    pub(crate) corner: Option<crate::overlay::Overlay>,
    /// The corner window being opened, until it lands.
    pub(crate) corner_pending: Option<gpui::Bounds<gpui::Pixels>>,
    /// D6: the Page screen's float (pill, notes panel) over its native page.
    pub(crate) page_float: Option<crate::overlay::Overlay>,
    pub(crate) page_float_pending: Option<gpui::Bounds<gpui::Pixels>>,
    /// The open tab menu's anchor: the clicked row's bounds, reported by
    /// the row's own prepaint. The popup seats at this rect; cleared with
    /// the menu, refilled on the next pane repaint.
    pub(crate) tab_menu_bounds: Option<(u64, Bounds<Pixels>)>,
    /// A session-owned tab the Browse centre shows: a task-folder row, or
    /// the session's page carried over from the pane. It stays owned by its
    /// session and survives a mode switch. Cleared by any person-tab select,
    /// a new session and a project or space change. Part of the sidebar key
    /// (and read by the centre every frame).
    pub(crate) browse_session_tab: Option<crate::browser::TabId>,
}

impl BrowseState {
    /// Empty: no tab and no webview exists until the Browser kind first
    /// shows. Tabs load lazily on first use (see `ensure_tabs_loaded`).
    pub(crate) fn new(fake: bool, cx: &mut gpui::App) -> Self {
        Self {
            states: HashMap::new(),
            favicons: favicons::Favicons::default(),
            fake,
            drop_cover: false,
            shot_pending: HashSet::new(),
            tabs: Tabs::default(),
            tabs_loaded: false,
            agent_tabs: HashMap::new(),
            centre_page_rect: None,
            progress_hole: Default::default(),
            holes_on: None,
            histories: HashMap::new(),
            find_strips: chrome::FindStrips::default(),
            visit_allows: chrome::VisitAllows::default(),
            permission_origins: HashMap::new(),
            downloads_open: false,
            downloads_anchor: None,
            extensions_open: false,
            downloads_rendered: false,
            more_open: false,
            fake_scripts: Vec::new(),
            download_started: HashMap::new(),
            last_centre_shown: None,
            find_resync: None,
            data: cx.new(super::data::BrowserData::new),
            recorder: history::Recorder::default(),
            text_reads: HashMap::new(),
            next_text_read: 1 << 62,
            cosmetic_reads: HashMap::new(),
            cosmetic_injects: std::collections::HashSet::new(),
            cosmetic_pass: HashMap::new(),
            dwell_armed: None,
            field_scope: None,
            placeholder_scoped: false,
            peek: None,
            split: None,
            split_ratio: 0.5,
            split_dragging: false,
            peek_focus_pending: false,
            split_focus: None,
            partner_held: false,
            task_seen_working: std::collections::HashSet::new(),
            task_done_at: std::collections::HashMap::new(),
            task_done_timer: None,
            browser_builds: 0,
            browser_idle_warmed: false,
            browser_url_focus_armed: false,
            browser_pending_url: None,
            browse_address: None,
            browse_address_sub: None,
            browse_omnibox_selected: 0,
            browse_pickup_hover: None,
            newtab_ground: None,
            site_info_open: false,
            bookmark_edit: None,
            bookmark_edit_sub: None,
            newtab_ground_asked: false,
            browse_omnibox_moved: false,
            back_bounds: None,
            browse_last_holds: false,
            browse_address_page_focus: false,
            browse_find: None,
            browse_find_sub: None,
            pending_ask_draft: None,
            browse_completed: None,
            browse_completed_text: None,
            pending_external_urls: Vec::new(),
            default_browser_cache: None,
            default_browser_confirm_pending: false,
            default_browser_recheck_at: None,
            browse_drop_target: None,
            browse_drop_at_end: false,
            tab_menu: None,
            browse_menu: None,
            page_menu_closed: None,
            browse_naming: None,
            overlay: None,
            overlay_pending: None,
            corner: None,
            corner_pending: None,
            page_float: None,
            page_float_pending: None,
            tab_menu_bounds: None,
            browse_session_tab: None,
        }
    }
}
