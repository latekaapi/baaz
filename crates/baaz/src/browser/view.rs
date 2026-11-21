//! Browse mode as its own entity (task 4.5, audit R4): [`BrowserView`]
//! owns every webview and the tab model, the Field and find inputs, the
//! Browse sidebar's menus and drags, the overlay windows over the native
//! page, the agent window, the `browser.db` service ([`super::data`]) and the
//! one owner of what covers the native page ([`super::occlusion`]).
//!
//! The view follows its pages itself: [`BrowserView::adopt_page`] observes
//! each live `WebviewState`, and a page's change runs the page reconcile
//! ([`BrowserView::reconcile_pages`] in `sync.rs`: tab URLs and titles,
//! favicons, history, back/forward, popups, downloads, permissions, the
//! debounced tabs write) with no frame drawn. What only the root can do —
//! a session's stored URL, a screenshot for the composer, the agent tools'
//! registry, the done task folders — goes out as a [`BrowserEvent`] the root
//! maps (`Harness::on_browser_event`). It never reaches back into `Harness`.
//!
//! As a [`Region`](crate::region::Region) it draws the Browse centre's
//! loading line, the part of the centre that only the view reads; the rest of
//! the centre (the page, the new-tab page, the strips and popovers) is still
//! composed by the root's `render_browse_centre`, whose handlers act on
//! session state as well.

use std::collections::{HashMap, HashSet};

use aui_webview::WebviewState;
use aui_tokens::{scale, ActiveAui};
use gpui::{div, prelude::*, px, AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Render, SharedString, Subscription, Window};

use super::{chrome, history, peek_split, TabId, Tabs};
use super::favicons;
use super::occlusion::NativeOcclusion;

/// What the view asks of the root (see the module doc).
#[derive(Clone, Debug)]
pub(crate) enum BrowserEvent {
    /// The live pages moved (a navigation, a title, a popup, a screenshot,
    /// an answer): the root runs its half of the reconcile
    /// (`Harness::reconcile_browser_root`). `session_urls` are the session
    /// owners' active tabs at their live URLs, for the stored
    /// `browser_url` each session restores from.
    PagesChanged { session_urls: Vec<(String, String)> },
    /// The discard sweep's timer came due: the root drops long-hidden pages
    /// (it knows which tab the pane shows, and the agent tools' registry).
    DiscardDue,
    /// The next done task folder came due: the root sweeps them (it knows
    /// which sessions still work).
    DoneTasksDue,
    /// The occlusion pass saw the centre switch tabs (`left` → `shown`):
    /// find follows — the left tab stops highlighting and the shared input
    /// takes the shown tab's query, which needs the root's window.
    CentreSwitched { left: Option<TabId>, shown: Option<TabId> },
    /// A click into the Split's other half (it just took the keyboard): the
    /// root makes it the shown tab.
    SplitFocus(TabId),
    /// The centre page's rect, or the pane's shown page's, moved: the
    /// windows floating over the page (the corner, the agent line, the
    /// notes panel) follow it.
    PageRectMoved,
}

impl EventEmitter<BrowserEvent> for BrowserView {}

/// Browse mode's state and the entity that owns it (see the module doc).
/// One field on [`Harness`](crate::app::Harness).
pub(crate) struct BrowserView {
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
    /// registry is built before the session facts are read, so the first
    /// browser use loads.
    pub tabs_loaded: bool,
    /// The tab the agent tools act on per session key, so discard never
    /// drops the agent's active tab and switches re-register it.
    pub agent_tabs: HashMap<String, TabId>,
    /// The Browse centre page's rect, stored by the frame's occlusion
    /// ([`Self::occlude`]): the mouse-down hand-back measures against this
    /// same rect (see `browse_centre_page_rect`), never a second
    /// computation. `None` outside Browse and before the first frame.
    pub centre_page_rect: Option<Bounds<Pixels>>,
    /// The pane's shown page's rect as the last occlusion pass saw it: the
    /// Page float anchors to it (a change is [`BrowserEvent::PageRectMoved`]).
    pub pane_page_rect: Option<Bounds<Pixels>>,
    /// Every tab's find strip (B2a-H5): per tab, so switching hides the
    /// strip and returning restores the query.
    pub find_strips: chrome::FindStrips,
    /// Origins trusted "while visiting" (B2a-H5): in memory for this run
    /// only — persistence is B2b.
    pub visit_allows: chrome::VisitAllows,
    /// Each live tab's last seen origin: navigating to another origin (or
    /// closing) revokes that origin's "while visiting" grants.
    pub permission_origins: HashMap<TabId, String>,
    /// The window's overlay stack ([`crate::overlay_stack`]): which Browse
    /// chrome popover (downloads, extensions), Browse menu, site
    /// information or bookmark popover stands open lives there, not here.
    pub(crate) overlays: gpui::Entity<crate::overlays::Overlays>,
    /// Where the sidebar's downloads button is (D4), in window
    /// coordinates: the popover opens under it.
    pub downloads_anchor: Option<gpui::Bounds<gpui::Pixels>>,
    /// Whether the downloads popover has rendered since it opened: an
    /// emptied list clears the popover only after it rendered once, so a
    /// scripted download's first frame never closes its own capture.
    pub downloads_rendered: bool,
    /// Scripted fake-backend events, queued by `--steps` verbs and consumed
    /// when the next scripted page builds (captures only; production never
    /// queues). The events drain through the real poll path, so captures
    /// show the same strips a live page would.
    pub fake_scripts: Vec<chrome::FakePageScript>,
    /// When each live download started arriving, as ms since the epoch
    /// ([`tabs::now_ms`]): the popover's "6 s left" estimates read this.
    /// Stamped on first sight, pruned when the download ends.
    pub download_started: HashMap<(TabId, u32), u64>,
    /// The centre tab the last frame's occlusion saw: a switch dismisses the chrome
    /// popovers (they point at the old tab's state) but never the find
    /// strip, which is per tab and restores on return.
    pub last_centre_shown: Option<TabId>,
    /// R4 `browser.db` as a service (task 4.5): history, page words and
    /// bookmarks, run on the background executor behind an in-memory front
    /// the UI reads ([`super::data::BrowserData`]).
    pub data: Entity<super::data::BrowserData>,
    /// Which page each person tab was recorded at, and its dwell.
    pub recorder: history::Recorder,
    /// Page-word reads in flight: request id → (tab, url, title).
    pub text_reads: HashMap<u64, (TabId, String, String)>,
    /// Selection reads in flight (docs/82 8.3), by request id: the tab
    /// whose page was asked for its selected text.
    pub(crate) selection_reads: HashMap<u64, TabId>,
    /// Each live page's selected text, as its last answer gave it: what
    /// "Send selection to session…" sends, and whether it is offered. Asked
    /// again whenever a menu or the Field opens on the tab. Never persists.
    pub(crate) selections: HashMap<TabId, String>,
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
    /// Whether the Split partner held the keyboard last frame.
    pub partner_held: bool,
    /// Done closes tabs (docs/27 §4.3): sessions this run saw working, and
    /// when each task folder went done.
    pub(crate) task_seen_working: std::collections::HashSet<String>,
    pub(crate) task_done_at: std::collections::HashMap<String, u64>,
    /// Wakes the done task folders' sweep when the next is due to close
    /// ([`BrowserEvent::DoneTasksDue`]).
    pub(crate) task_done_timer: Option<gpui::Task<()>>,
    /// How many webviews the open actions built (B7fix2): the spy the
    /// render-must-not-construct test reads. Incremented wherever a
    /// webview is actually constructed, never when an existing one is
    /// reused — so one open action plus any number of frames stays one.
    pub(crate) browser_builds: u64,
    /// Whether the one idle-time Browser warmup already ran (B7fix2): set
    /// the first time the shell is up with a session open
    /// (`Harness::maybe_idle_prewarm`), so no frame constructs even when a
    /// restore left the pane open on Browser.
    pub(crate) browser_idle_warmed: bool,
    /// The person's own Browser open still owes the URL field its focus
    /// (Z7a2). Armed by `show_right`/`toggle_right` when they land open on
    /// Browser, consumed once by the webview ensure that follows — the
    /// root's settle (`Harness::settle`), else the pane's next compose — and
    /// only there, so activation, restore and boot never steal the keyboard
    /// into the URL field. Never persists.
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
    pub(crate) newtab_ground_asked: bool,
    /// Whether the highlight moved off the top Ask row (B2a-H4): Enter
    /// activates the highlighted row once moved, and commits the raw text
    /// otherwise — so plain Enter navigates instead of asking. Set by
    /// `move_omnibox`, cleared by typing and whenever editing ends. Never
    /// persists.
    pub(crate) browse_omnibox_moved: bool,
    /// Whether the Browse centre's page held the keyboard on the last
    /// frame's occlusion ([`Self::occlude`], B2a-H4 7b): the rising edge ends address editing,
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
    /// Content picked for a session whose view has not landed (docs/82
    /// 8.2, [`super::parcels`]): a screenshot, a page's notes, a hand-off.
    /// `activate` writes each into its own session's composer — never sent,
    /// never on the wrong session. Keyed by session id; a new session's
    /// parcel is untargeted while its open is in flight (never taken by an
    /// unrelated landing) and dropped on every early return. Taken on
    /// landing. Never persists.
    pub(crate) parcels: Vec<super::parcels::PendingParcel>,
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
    /// only — never a live LaunchServices lookup — and
    /// [`Self::refresh_default_browser_cache`] refreshes it at launch, on
    /// focus-regain (the root's window-activation observer) and when the
    /// Make-default re-read comes due. Never persists.
    pub(crate) default_browser_cache: Option<Option<String>>,
    /// A Make-default click is waiting for macOS to confirm: the row
    /// shows "Waiting for macOS to confirm…" until the next re-read.
    /// Never persists.
    pub(crate) default_browser_confirm_pending: bool,
    /// The post-Make-default re-read (macOS confirms asynchronously —
    /// about a second after the click — plus again on the next
    /// focus-regain): a timer the view owns. Never persists.
    pub(crate) default_browser_recheck: Option<gpui::Task<()>>,
    /// The tab row a drag hovers over: the 2 px accent line's slot while
    /// a tab drag is in flight. Transient (cleared on drop), never keyed.
    pub(crate) browse_drop_target: Option<crate::browser::TabId>,
    /// The drop-at-end slot below the last tab row: the accent line's
    /// slot when a tab drag rests past every row (drops append at the
    /// end). Transient like the row target, never keyed.
    pub(crate) browse_drop_at_end: bool,
    /// The Browse name field in flight (a folder or a space), edited in
    /// the shared `rename` textarea.
    pub(crate) browse_naming: Option<aui::screens::sidebar::BrowseNaming>,
    /// The overlay window over the native page (Browse menus, the Field,
    /// the hosted modal), while it is open or opening.
    pub(crate) overlay: Option<crate::overlay::Overlay>,
    /// The corner window (Browse's corner stack), beside the overlay.
    pub(crate) corner: Option<crate::overlay::Overlay>,
    /// D6: the Page screen's float (pill, notes panel) over its native page.
    pub(crate) page_float: Option<crate::overlay::Overlay>,
    /// A session-owned tab the Browse centre shows: a task-folder row, or
    /// the session's page carried over from the pane. It stays owned by its
    /// session and survives a mode switch. Cleared by any person-tab select,
    /// a new session and a project or space change. Part of the sidebar key
    /// (and read by the centre every frame).
    pub(crate) browse_session_tab: Option<crate::browser::TabId>,
    /// The agent window over the page (task 4.5): the ⌘J line, the ⌘E card,
    /// its float and handoff picker, and the browser chats' pages and
    /// handoffs ([`crate::app::agent_window::AgentWindowState`]).
    pub(crate) agent_window: crate::app::agent_window::AgentWindowState,
    /// The one owner of what covers the native pages, which shows, and the
    /// holes cut through the centre's ([`NativeOcclusion`]).
    pub(crate) occlusion: NativeOcclusion,
    /// One observation per live page ([`Self::adopt_page`]): a page's change
    /// runs the page reconcile. Pruned with the pages.
    pub(super) page_watch: HashMap<TabId, Subscription>,
    /// The open Peek's page, observed like a tab's ([`Self::adopt_peek`]):
    /// a link gesture inside it drains on its change.
    pub(super) peek_watch: Option<Subscription>,
    /// The `browser.db` service's observation: a landing redraws the view.
    _data_watch: Subscription,
    /// The page reconcile's wake-ups (dwell, ad-box second pass, answers
    /// still out on a hidden page), owned here so they die with the view.
    pub(super) reconcile_timer: Option<(std::time::Instant, gpui::Task<()>)>,
    /// A reconcile is queued for the next turn ([`Self::adopt_page`]).
    pub(super) reconcile_queued: bool,
    /// The discard sweep's timer: hidden pages idle past the timeout are
    /// dropped though no page changes ([`BrowserEvent::DiscardDue`]).
    pub(super) discard_timer: Option<gpui::Task<()>>,
    /// The region's focus handle (the region contract); the centre's own
    /// inputs keep theirs.
    focus: FocusHandle,
}

impl BrowserView {
    /// Empty: no tab and no webview exists until the Browser kind first
    /// shows. Tabs load lazily on first use (see `ensure_tabs_loaded`).
    pub(crate) fn new(fake: bool, overlays: gpui::Entity<crate::overlays::Overlays>, cx: &mut gpui::Context<Self>) -> Self {
        let data = cx.new(super::data::BrowserData::new);
        // A `browser.db` landing (hits, history length, bookmarks) changes
        // what the Field, the ★ and the sidebar draw: redraw the view, never
        // the root (task 4.5b).
        let data_watch = cx.observe(&data, |_, _, cx| cx.notify());
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
            pane_page_rect: None,
            find_strips: chrome::FindStrips::default(),
            visit_allows: chrome::VisitAllows::default(),
            permission_origins: HashMap::new(),
            overlays,
            downloads_anchor: None,
            downloads_rendered: false,
            fake_scripts: Vec::new(),
            download_started: HashMap::new(),
            last_centre_shown: None,
            data,
            recorder: history::Recorder::default(),
            text_reads: HashMap::new(),
            selection_reads: HashMap::new(),
            selections: HashMap::new(),
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
            newtab_ground_asked: false,
            browse_omnibox_moved: false,
            browse_last_holds: false,
            browse_address_page_focus: false,
            browse_find: None,
            browse_find_sub: None,
            parcels: Vec::new(),
            browse_completed: None,
            browse_completed_text: None,
            pending_external_urls: Vec::new(),
            default_browser_cache: None,
            default_browser_confirm_pending: false,
            default_browser_recheck: None,
            browse_drop_target: None,
            browse_drop_at_end: false,
            browse_naming: None,
            overlay: None,
            corner: None,
            page_float: None,
            browse_session_tab: None,
            agent_window: crate::app::agent_window::AgentWindowState::default(),
            occlusion: NativeOcclusion::default(),
            page_watch: HashMap::new(),
            peek_watch: None,
            _data_watch: data_watch,
            reconcile_timer: None,
            reconcile_queued: false,
            discard_timer: None,
            focus: cx.focus_handle(),
        }
    }
}

impl BrowserView {
    /// The page's loading line (owner 2026-10-11; canvas BrowseFixes): 2 px,
    /// the foreground filling a border-12 % track in proportion to the
    /// engine's progress. Flat (docs/83 §3): the line runs the page column's
    /// top edge, end to end (the page has no corners), inside the page's
    /// rect — it is cut through the page as a hole while it shows
    /// ([`Self::cut_holes`]), since the native page covers anything gpui
    /// draws under it.
    fn render_page_progress(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.occlusion.progress_hole.set(None);
        let state = self.browse_centre_shown(cx).and_then(|id| self.states.get(&id))?;
        let (loading, progress) = {
            let page = state.read(cx);
            (page.loading(), page.progress())
        };
        if !loading {
            return None;
        }
        use aui::util::ElementExt as _;
        let hole = self.occlusion.progress_hole.clone();
        let p = cx.aui().colors;
        let line = px(scale::SP_1);
        let fill = progress.clamp(0.0, 1.0);
        Some(
            div()
                .id("page-progress")
                .role(gpui::Role::ProgressIndicator)
                .aria_label("Loading page")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(line)
                .overflow_hidden()
                .bg(p.line)
                .flex()
                .flex_row()
                .child(div().h_full().bg(p.fg).flex_grow(fill))
                .child(div().h_full().flex_grow(1.0 - fill))
                .on_prepaint(move |bounds, _, _| hole.set(Some(bounds)))
                .into_any_element(),
        )
    }
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl crate::region::Region for BrowserView {
    /// The Browse centre's landmark name.
    fn title(&self, _cx: &App) -> SharedString {
        "Browse".into()
    }

    /// Browse's header row is the root's bar (the tab's crumb, the mode
    /// switch): the view adds nothing there yet.
    fn actions(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    /// The view draws over its own centre page — a chrome popover or the
    /// crashed page — as the last frame's occlusion answer said.
    fn covers_page(&self, _cx: &App) -> bool {
        self.occlusion.shown().centre_covered_by_view
    }
}

impl Render for BrowserView {
    /// The part of the Browse centre the view draws itself: the loading
    /// line over the page column's top edge. Nothing (an empty, absolute
    /// box) while no page loads.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_page_progress(cx).unwrap_or_else(|| div().absolute().into_any_element())
    }
}
