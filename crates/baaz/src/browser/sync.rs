//! The Browse reconciliation (task 4.5): the centre page's geometry, and the
//! page reconcile that follows every live page.
//!
//! The reconcile runs from observation, never from render (task 4.5b, audit
//! F13/F19): the view observes each `WebviewState` it holds
//! ([`BrowserView::adopt_page`]), and a page's change runs
//! [`BrowserView::reconcile_pages`] — tab URLs, titles and favicons with the
//! debounced tabs write, history and page words, the ad-box cosmetics,
//! back/forward, popups, downloads, permissions. Its wake-ups for what no
//! page reports (the dwell, the cosmetics' second pass, answers still out on
//! a hidden page, the discard sweep, the done task folders) are timers the
//! view owns. The root's half — a session's stored URL, link gestures,
//! screenshots for the composer, the discard's tool registry, the done task
//! folders — runs on the view's [`BrowserEvent`]
//! ([`Harness::on_browser_event`]). Which page shows, and what covers it, is
//! decided per frame by the one occlusion owner (`occlusion.rs`).

use super::*;

impl Harness {

    /// Whether the centre's tab itself is blank (its stored URL): a tab
    /// mid-way through its first navigation is not, even while its live
    /// page still reports `about:blank`.
    pub(crate) fn browse_centre_tab_blank(&self, cx: &gpui::App) -> bool {
        self.browser.read(cx).browse_centre_shown(cx)
            .and_then(|id| self.browser.read(cx).tabs.get(id))
            .is_none_or(|tab| tab.url.is_empty() || tab.url == BLANK)
    }

    /// The Browse centre's live page, when one already exists — the
    /// shown tab's state — never creating. The mouse-down hand-back
    /// reads this instead of the pane's page while in Browse.
    pub(crate) fn browse_centre_state(&self, cx: &gpui::App) -> Option<Entity<WebviewState>> {
        let id = self.browser.read(cx).browse_centre_shown(cx)?;
        self.browser.read(cx).states.get(&id).cloned()
    }

    /// The open terminal dock's height in window pixels, else zero
    /// (B2a-H2b): the one height behind the centre rect, the occlusion's
    /// dock bounds and the mouse-down hand-back — computed once here,
    /// reused everywhere, so the three can never disagree.
    pub(super) fn open_dock_height_px(&self, window: &Window, cx: &gpui::App) -> Pixels {
        if !crate::layout::Layout::get(cx).terminal_open {
            return gpui::px(0.0);
        }
        // The centre column runs the window's height (the flat shell has
        // no inset, docs/83 §3).
        let centre_height = f32::from(window.bounds().size.height).max(0.0);
        let want =
            crate::layout::Layout::get(cx).terminal_height.unwrap_or(crate::terminal::DOCK_DEFAULT_HEIGHT);
        gpui::px(crate::terminal::clamp_dock_height(want, centre_height))
    }

    /// The Browse centre column's rect for the window's `content` box:
    /// past the sidebar when open, short of the right pane when open,
    /// above the open dock. The dock stacks below the centre in the
    /// same `v_flex` — beside it, never over it — so its height
    /// subtracts here, and the column the `webview_pane` fills is
    /// exactly this rect.
    pub(super) fn browse_centre_content_rect(&self, content: Bounds<Pixels>, window: &Window, cx: &gpui::App) -> Bounds<Pixels> {
        let sidebar = if crate::layout::Layout::get(cx).sidebar_open {
            gpui::px(self.resize.width)
        } else {
            gpui::px(0.0)
        };
        let right = if crate::layout::Layout::get(cx).right_open {
            gpui::px(self.right_width(cx))
        } else {
            gpui::px(0.0)
        };
        browser_centre_bounds(content, sidebar, right, self.open_dock_height_px(window, cx))
    }

    /// The Browse centre page's rect for the window's `content` box: the
    /// whole column above — the centre page draws with
    /// [`WebviewPane::without_nav`](aui_webview::WebviewPane::without_nav),
    /// so no nav row offsets it. The single source behind placement and
    /// release: the frame's occlusion stores it ([`BrowserView::occlude`],
    /// [`BrowserView::centre_page_rect`]) and the mouse-down
    /// hand-back reads the stored rect, so the slide — a translate
    /// inside the clipped sidebar, which moves no column — changes
    /// nothing either side computes.
    pub(super) fn browse_centre_page_rect(&self, content: Bounds<Pixels>, window: &Window, cx: &gpui::App) -> Bounds<Pixels> {
        // Flat (docs/83 §3): the page is the centre column edge to edge —
        // between the sidebar column and the right column, the dock below
        // it inside the column.
        if !self.settings_open(cx) {
            let sidebar = if crate::layout::Layout::get(cx).sidebar_open { gpui::px(self.resize.width) } else { gpui::px(0.0) };
            let mut right = if crate::layout::Layout::get(cx).right_open { gpui::px(self.right_width(cx)) } else { gpui::px(0.0) };
            // The docked agent column takes its strip beside the page (R3):
            // the page gives it up rather than sitting under it (the card
            // window puts the column 6 past the page's right edge).
            if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse && self.browser.read(cx).agent_window.card_ui == crate::app::agent_window::CardUi::Column {
                right += gpui::px(crate::app::agent_window::COLUMN_W + 6.0);
            }
            return column_page_rect(content, sidebar, right, self.open_dock_height_px(window, cx));
        }
        self.browse_centre_content_rect(content, window, cx)
    }

    /// The Browse centre's page (B2a-H2b): the current project's person
    /// tab, when one is open — never creating. Tab creation lives on
    /// the events that need it (entering Browse, closing the last
    /// person tab, the New-tab affordances), never in render: with no
    /// tab the centre draws its empty state instead. The page is built
    /// here, where the centre shows it — the centre twin of
    /// [`Self::browser_for`].
    pub(crate) fn browse_centre_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<WebviewState>> {
        self.ensure_tabs_loaded(cx);
        let id = self.browser.read(cx).browse_centre_shown(cx)?;
        Some(self.ensure_tab_page(HOME_KEY, id, window, cx))
    }

    /// Drop hidden tabs' live pages. On the sweep (`force` false) only
    /// tabs idle past the timeout go; `tab-discard-now` forces every hidden
    /// page at once. Audible pages, pinned tabs and the agent's active
    /// tabs are always spared; the shown tabs — the pane's and Browse's
    /// centre — are never candidates. Showing a dropped tab again
    /// rebuilds its page at its URL. The agent tools' registry is the
    /// root's, so the root drops the registrations.
    pub(super) fn discard_hidden_tabs(
        &mut self,
        force: bool,
        shown: Option<TabId>,
        centre: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        let (dropped, stale) = self.browser.update(cx, |browser, cx| browser.discard_hidden_pages(force, shown, centre, cx));
        for id in &dropped {
            // The tools' registry would keep the discarded page alive (and
            // on screen, and uncollected) — see `close_tab`.
            self.terminal_service.unregister_tab_page(id.0);
        }
        for key in stale {
            self.terminal_service.unregister_browser(&key);
        }
    }

    /// One event from the Browse view, mapped to the root's own method.
    pub(crate) fn on_browser_event(
        &mut self,
        _browser: &Entity<BrowserView>,
        event: &BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            BrowserEvent::PagesChanged { session_urls } => self.reconcile_browser_root(session_urls, window, cx),
            BrowserEvent::DiscardDue => {
                let shown = self.browser.read(cx).occlusion.shown();
                self.discard_hidden_tabs(false, shown.pane, shown.centre, cx);
                self.browser.update(cx, |browser, cx| browser.arm_discard(cx));
            }
            BrowserEvent::DoneTasksDue => self.close_done_task_tabs(cx),
        }
    }

    /// The root's half of the page reconcile (see the module doc): what
    /// the live pages changed that only the root can act on.
    fn reconcile_browser_root(&mut self, session_urls: &[(String, String)], window: &mut Window, cx: &mut Context<Self>) {
        // The session's active tab keeps writing `browser_url`, so older
        // builds still restore — debounced to actual changes, never one
        // write per poll. B7: a navigation is not searchable, so it
        // persists through the cheap debounced write — never the
        // session-list settle.
        for (session, current) in session_urls {
            let stored = self
                .overrides
                .get(session)
                .and_then(|meta| meta.right.clone())
                .and_then(|right| right.browser_url);
            if should_remember(current, stored.as_deref()) {
                self.remember_browser_url_cheap(session, current.clone(), cx);
            }
        }
        // ⌘-, ⇧- and ⌥-clicks the page script reported: new tabs, a Peek,
        // a Split (the Peek and Split build their page with the window).
        self.drain_link_gestures(window, cx);
        // A Screenshot whose bytes were not there yet attaches on arrival.
        // Pending captures key to the tab they were taken on
        // ([`screenshot_tab_key`]): the bytes attach to the active
        // session's draft only while that tab still shows, so a switch
        // can never misattribute them.
        let active_key = self.browser_key(cx);
        let shown_now = self.browser.read(cx).browse_centre_shown(cx);
        let pending: Vec<String> = self.browser.read(cx).shot_pending.iter().cloned().collect();
        for key in pending {
            if let Some(id) = screenshot_tab_id(&key) {
                let ready = self
                    .browser
                    .read(cx)
                    .states
                    .get(&id)
                    .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec));
                if let Some(bytes) = ready {
                    if shown_now == Some(id) {
                        self.browser.update(cx, |browser, _| browser.shot_pending.remove(&key));
                        self.attach_browser_screenshot(bytes, cx);
                    }
                }
                continue;
            }
            let owner = self.browser.read(cx).tab_owner_for_key(&key);
            let space = self.browser.read(cx).owner_space(&owner);
            let bytes = self
                .browser
                .read(cx)
                .tabs
                .active_of(&owner, space.as_deref())
                .and_then(|id| self.browser.read(cx).states.get(&id))
                .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec));
            if let Some(bytes) = bytes {
                if key == active_key {
                    self.browser.update(cx, |browser, _| browser.shot_pending.remove(&key));
                    self.attach_browser_screenshot(bytes, cx);
                }
            }
        }
        // Long-hidden pages drop their native views (but keep their tabs);
        // the shown tabs are never candidates.
        let shown = self.browser.read(cx).occlusion.shown();
        self.discard_hidden_tabs(false, shown.pane, shown.centre, cx);
        self.browser.update(cx, |browser, cx| browser.arm_discard(cx));
        // Task folders done for 15 minutes close their tabs (R3).
        self.close_done_task_tabs(cx);
    }
}

impl BrowserView {
    /// The Browse centre's shown tab, when Browse shows one (B2a-H2): the
    /// session tab it holds, else the current space's person-tab page. `None`
    /// in Sessions mode, before the tabs load, or with no tab yet.
    pub(crate) fn browse_centre_shown(&self, cx: &gpui::App) -> Option<TabId> {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse || !self.tabs_loaded {
            return None;
        }
        // A session-owned tab the centre holds: ownership never moves, and
        // any person-tab select clears it back to the space's own tab.
        if let Some(id) = self.browse_session_tab {
            if self.tabs.get(id).is_some_and(|tab| matches!(tab.owner, Owner::Session(_))) {
                return Some(id);
            }
        }
        let space = self.browse_space();
        self.tabs.active_of(&Owner::Person, space.as_deref())
    }

    /// Take `state` as tab `id`'s live page and follow it: every change the
    /// page reports runs [`Self::reconcile_pages`], frame or no frame. The
    /// one way a page joins [`Self::states`].
    pub(crate) fn adopt_page(&mut self, id: TabId, state: Entity<WebviewState>, cx: &mut Context<Self>) {
        self.page_watch.insert(id, cx.observe(&state, |this, _, cx| this.reconcile_pages(cx)));
        self.states.insert(id, state);
        // A page built at its URL has nothing to report yet: reconcile it
        // once on the next turn, so it reaches its tab and history now.
        self.reconcile_soon(cx);
    }

    /// Run the page reconcile on the next turn (once, however many ask).
    fn reconcile_soon(&mut self, cx: &mut Context<Self>) {
        if self.reconcile_queued {
            return;
        }
        self.reconcile_queued = true;
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                this.reconcile_queued = false;
                this.reconcile_pages(cx);
            });
        })
        .detach();
    }

    /// Open `peek` over the centre and follow its page like a tab's: a link
    /// gesture inside the Peek drains on the page's change.
    pub(crate) fn adopt_peek(&mut self, peek: peek_split::Peek, cx: &mut Context<Self>) {
        self.peek_watch = Some(cx.observe(&peek.state, |this, _, cx| this.reconcile_pages(cx)));
        self.peek = Some(peek);
    }

    /// The page reconcile (see the module doc): pull the live pages into
    /// their tabs and the feature state that follows them, then tell the
    /// root ([`BrowserEvent::PagesChanged`]). Runs on a page's change and
    /// on the view's own timers — never from render. No SQLite here: the
    /// history and page words queue to the `browser.db` worker.
    pub(crate) fn reconcile_pages(&mut self, cx: &mut Context<Self>) {
        if !self.tabs_loaded {
            return;
        }
        let states = &self.states;
        self.page_watch.retain(|id, _| states.contains_key(id));
        // Live pages report into their tabs — debounced to actual changes,
        // never one write per poll.
        let live: Vec<(TabId, String, String, Option<String>)> = self
            .states
            .iter()
            .map(|(id, state)| {
                let page = state.read(cx);
                (*id, page.url().to_string(), page.title().to_string(), page.favicon().map(|url| url.to_string()))
            })
            .collect();
        let mut tabs_dirty = false;
        for (id, url, title, favicon) in &live {
            if self.tabs.set_url_title(*id, url, title) {
                tabs_dirty = true;
            }
            if let Some(favicon) = favicon {
                if self.tabs.set_favicon(*id, favicon) {
                    tabs_dirty = true;
                }
            }
        }
        self.fetch_favicons(cx);
        // The session owners' active tabs at their live URLs, for the root
        // to store as each session's `browser_url`.
        let urls: HashMap<TabId, String> = live.iter().map(|(id, url, _, _)| (*id, url.clone())).collect();
        let session_urls: Vec<(String, String)> = self
            .tabs
            .active
            .values()
            .filter_map(|id| {
                let tab = self.tabs.get(*id)?;
                let Owner::Session(session) = &tab.owner else { return None };
                Some((session.clone(), urls.get(id)?.clone()))
            })
            .collect();
        self.record_history(&live, cx);
        // The header toolbar's back/forward stacks follow every live page:
        // whatever the page reports becomes the stack's current URL, so an
        // outside navigation (a link) lands here and clears the forward
        // stack exactly like a fresh one. Whether a load runs is the
        // engine's own `WebviewState::loading`, read where the stop/reload
        // control needs it — never tracked here.
        for (id, url, _, _) in &live {
            self.histories.entry(*id).or_default().note_navigated(url);
        }
        // B2a-H5: popups become tabs (foreground ones select, so the centre
        // follows), downloads stamp their start for the popover's
        // estimates, and closed tabs drop their find strips.
        if self.drain_browse_popups(cx) {
            tabs_dirty = true;
        }
        self.prune_split();
        self.stamp_browse_downloads(cx);
        let live: HashSet<TabId> = self.tabs.tabs.iter().map(|tab| tab.id).collect();
        self.find_strips.retain(&live);
        // A closed tab drops its strip above: with none standing open the
        // shared input has no reader, so it drops too — the same hand-over
        // `close_browse_find` does for Done/Esc.
        if !self.find_strips.any_open() {
            self.browse_find = None;
            self.browse_find_sub = None;
        }
        // Tabs that navigated away (or closed) drop their "while visiting"
        // grants; then trusted origins auto-answer and background asks
        // mark their rows while in Browse.
        let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let centre = self.browse_centre_shown(cx);
        self.revoke_stale_visit_grants();
        self.route_browse_permissions(centre, in_browse, cx);
        if tabs_dirty {
            self.schedule_tabs_write();
            // The sidebar's rows and the header read the tabs: redraw.
            cx.notify();
        }
        cx.emit(BrowserEvent::PagesChanged { session_urls });
    }

    /// Run the page reconcile again after `wait` unless an earlier run is
    /// already due: what no page reports on its own (the dwell, the
    /// cosmetics' second pass, an answer from a hidden page) comes due here.
    fn reconcile_after(&mut self, wait: std::time::Duration, cx: &mut Context<Self>) {
        let due = std::time::Instant::now() + wait;
        if self.reconcile_timer.as_ref().is_some_and(|(at, _)| *at <= due) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.reconcile_timer = None;
                this.reconcile_pages(cx);
            });
        });
        self.reconcile_timer = Some((due, task));
    }

    /// Ad blocking's element hiding (owner, 12 Nov: blocked ads left empty
    /// boxes): each loaded http(s) page is scanned for its classes and ids
    /// once it stops loading, and again 2.5 s later for slots filled in late;
    /// the matching rules go in as one stylesheet. Cheap: a map look-up per
    /// live tab.
    fn adblock_cosmetics(&mut self, live: &[(TabId, String, String, Option<String>)], cx: &mut Context<Self>) {
        const SECOND_PASS: std::time::Duration = std::time::Duration::from_millis(2_500);
        let now = std::time::Instant::now();
        self.cosmetic_pass.retain(|id, _| live.iter().any(|(tab, ..)| tab == id));
        for (id, url, ..) in live {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                continue;
            }
            let Some(state) = self.states.get(id).cloned() else { continue };
            if state.read(cx).loading() || self.cosmetic_reads.values().any(|(tab, _)| tab == id) {
                continue;
            }
            let pass = match self.cosmetic_pass.get(id) {
                Some((seen, n, _)) if seen == url && *n >= 2 => continue,
                Some((seen, 1, first)) if seen == url => {
                    let since = now.duration_since(*first);
                    if since < SECOND_PASS {
                        // No page reports the second pass coming due.
                        self.reconcile_after(SECOND_PASS - since + std::time::Duration::from_millis(200), cx);
                        continue;
                    }
                    2
                }
                Some((seen, ..)) if seen == url => continue,
                _ => 1,
            };
            let first = if pass == 1 { now } else { self.cosmetic_pass.get(id).map_or(now, |p| p.2) };
            self.cosmetic_pass.insert(*id, (url.clone(), pass, first));
            let request = self.next_text_read;
            self.next_text_read += 1;
            self.cosmetic_reads.insert(request, (*id, url.clone()));
            state.update(cx, |state, _| state.eval_with_result(request, adblock::COSMETIC_SCAN_JS));
            // A shown page reports its answer (it notifies); a hidden one
            // does not, so collect it on the view's own wake-up.
            self.reconcile_after(std::time::Duration::from_millis(1_200), cx);
        }
    }

    /// R4 history (docs/27 §7): every person tab's page becomes a visit, a
    /// late title updates it, and the page in front gives up its words once
    /// it has stayed [`history::DWELL_MS`]. A session's tabs never record.
    /// Runs from the page reconcile, so it must stay cheap: map lookups, and
    /// a write queued to the `browser.db` worker ([`super::data`]) only when
    /// a page or title changed — never a SQLite call here.
    fn record_history(&mut self, live: &[(TabId, String, String, Option<String>)], cx: &mut Context<Self>) {
        let now = tabs::now_ms();
        let front = self.last_centre_shown;
        let mut changed = false;
        let mut want: Vec<(TabId, String, String)> = Vec::new();
        let writer = self.data.read(cx).writer();
        for (id, url, title, _) in live {
            let person = matches!(self.tabs.get(*id).map(|tab| &tab.owner), Some(Owner::Person));
            let before = self.recorder.url_of(*id).map(str::to_owned);
            let note = self.recorder.observe(&writer, *id, person, url, title, front == Some(*id), now);
            // Only a page history keeps is a change: an unrecordable page
            // (a blank tab) is forgotten by the recorder on every pass, and
            // must not ask the Field's history again each time (its answer
            // lands and redraws: task 4.5).
            if before.as_deref() != Some(url.as_str()) && person && history::recordable(url) {
                changed = true;
            }
            if note == history::Note::WantText {
                want.push((*id, url.clone(), title.clone()));
            }
        }
        if changed {
            self.data.read(cx).history_changed();
        }
        self.recorder.retain(|id| live.iter().any(|(tab, ..)| *tab == id));
        for (id, url, title) in want {
            let Some(state) = self.states.get(&id).cloned() else { continue };
            let request = self.next_text_read;
            self.next_text_read += 1;
            crate::baaz_log!("history: reading the words of {url}");
            self.text_reads.insert(request, (id, url, title));
            // The page in front answers on its poll and notifies, which
            // runs this reconcile again to collect the words.
            state.update(cx, |state, _| state.eval_with_result(request, history::PAGE_TEXT_JS));
        }
        self.adblock_cosmetics(live, cx);
        // Answers: keep the words if the tab is still on that page. One
        // drain serves every script Baaz runs here (page words, ad-box
        // scans, stylesheet installs): taking a page's answers takes all of
        // them, so each is routed by its request id, never dropped.
        let pending = !self.text_reads.is_empty() || !self.cosmetic_reads.is_empty() || !self.cosmetic_injects.is_empty();
        if pending {
            let states: Vec<(TabId, Entity<WebviewState>)> = self.states.iter().map(|(id, state)| (*id, state.clone())).collect();
            for (id, state) in states {
                let waiting = self.text_reads.values().any(|(tab, _, _)| *tab == id)
                    || self.cosmetic_reads.values().any(|(tab, _)| *tab == id)
                    || !self.cosmetic_injects.is_empty();
                if !waiting {
                    continue;
                }
                let answers = state.update(cx, |state, _| state.take_eval_results());
                for (request, answer) in answers {
                    if self.cosmetic_injects.remove(&request) {
                        continue;
                    }
                    if let Some((tab, url)) = self.cosmetic_reads.remove(&request) {
                        if tab == id {
                            self.land_cosmetic_scan(&state, &url, answer, cx);
                        }
                        continue;
                    }
                    let Some((tab, url, title)) = self.text_reads.remove(&request) else { continue };
                    // The words were asked for while the tab showed `url`;
                    // a navigation since does not make them another page's.
                    if tab != id {
                        continue;
                    }
                    if let Err(error) = &answer {
                        crate::baaz_log!("history: no words for {url} ({error})");
                    }
                    if let Ok(text) = answer {
                        let text: String = serde_json::from_str(&text).unwrap_or(text);
                        crate::baaz_log!("history: kept {} characters of {url}", text.len());
                        self.data.update(cx, |data, _| data.set_text(&url, &title, &text));
                    }
                }
            }
        }
        // No page reports its dwell: wake once when the page in front has
        // stayed long enough, so its words are kept without input.
        if let Some(id) = front {
            let url = live.iter().find(|(tab, ..)| *tab == id).map(|(_, url, ..)| url.clone());
            if let Some(url) = url.filter(|url| history::recordable(url)) {
                if self.dwell_armed.as_ref() != Some(&(id, url.clone())) {
                    self.dwell_armed = Some((id, url));
                    self.reconcile_after(std::time::Duration::from_millis(history::DWELL_MS + 250), cx);
                }
            }
        }
    }

    /// A scan answered: install the stylesheet its classes and ids earn.
    pub(super) fn land_cosmetic_scan(&mut self, state: &Entity<WebviewState>, url: &str, answer: Result<String, String>, cx: &mut gpui::App) {
        let Ok(text) = answer else { return };
        // The script returns a JSON string; the channel may wrap it once more.
        let text: String = serde_json::from_str(&text).unwrap_or(text);
        #[derive(serde::Deserialize)]
        struct Scan {
            #[serde(default)]
            c: Vec<String>,
            #[serde(default)]
            i: Vec<String>,
        }
        let Ok(scan) = serde_json::from_str::<Scan>(&text) else { return };
        let Some(css) = adblock::page_css(url, &scan.c, &scan.i) else { return };
        crate::baaz_log!("adblock: hiding {} ad boxes' rules on {}", css.lines().count(), history::host_of(url));
        let request = self.next_text_read;
        self.next_text_read += 1;
        self.cosmetic_injects.insert(request);
        let js = adblock::inject_js(&css);
        state.update(cx, |state, _| state.eval_with_result(request, &js));
    }

    /// Drop hidden pages idle past the timeout (or all hidden pages when
    /// `force`), sparing the shown tabs, audible and pinned pages and the
    /// agent's tabs (see [`Harness::discard_hidden_tabs`]). Answers the
    /// dropped tabs and the agent keys whose registration died with them.
    pub(super) fn discard_hidden_pages(
        &mut self,
        force: bool,
        shown: Option<TabId>,
        centre: Option<TabId>,
        cx: &mut gpui::App,
    ) -> (Vec<TabId>, Vec<String>) {
        let now = tabs::now_ms();
        let agent: HashSet<TabId> = self.agent_tabs.values().copied().collect();
        let live: Vec<(TabId, bool, bool)> = self
            .states
            .iter()
            .map(|(id, state)| {
                let page = state.read(cx);
                let tab = self.tabs.get(*id);
                (*id, page.audible(), tab.is_some_and(|tab| tab.pinned))
            })
            .collect();
        let mut dropped = Vec::new();
        for (id, audible, pinned) in live {
            if Some(id) == shown || Some(id) == centre || audible || pinned || agent.contains(&id) {
                continue;
            }
            let last = self.tabs.get(id).and_then(|tab| tab.last_shown);
            // Forced (`tab-discard-now`) drops every hidden page at once;
            // the sweep only drops tabs idle past the timeout.
            let due = if force { true } else { tabs::should_discard(now, last, audible, false) };
            if due {
                dropped.push(id);
            }
        }
        for id in &dropped {
            self.states.remove(id);
            self.page_watch.remove(id);
        }
        // A discarded page can never stay registered with the agent tools:
        // agent tabs are spared above, so this fires only if that ever
        // stops holding — and then the registration dies with the page.
        let gone: HashSet<TabId> = dropped.iter().copied().collect();
        let stale: Vec<String> = self.agent_tabs.iter().filter(|(_, id)| gone.contains(id)).map(|(key, _)| key.clone()).collect();
        for key in &stale {
            self.agent_tabs.remove(key);
        }
        (dropped, stale)
    }

    /// Arm the discard sweep for the next hidden page to come due: no page
    /// reports going idle, so the view wakes itself and asks the root
    /// ([`BrowserEvent::DiscardDue`]), which knows the agent tools'
    /// registry.
    pub(crate) fn arm_discard(&mut self, cx: &mut Context<Self>) {
        if self.discard_timer.is_some() {
            return;
        }
        let shown = self.occlusion.shown();
        let now = tabs::now_ms();
        let timeout = tabs::discard_timeout_secs() * 1000;
        let next = self
            .states
            .keys()
            .filter(|id| Some(**id) != shown.pane && Some(**id) != shown.centre)
            .filter_map(|id| self.tabs.get(*id)?.last_shown)
            .map(|last| (last + timeout + 1_000).saturating_sub(now))
            .min();
        let Some(wait) = next else { return };
        self.discard_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
            let _ = this.update(cx, |this, cx| {
                this.discard_timer = None;
                cx.emit(BrowserEvent::DiscardDue);
            });
        }));
    }

    /// Arm the done task folders' sweep for `wait` ms from now, once: no
    /// page reports a folder coming due ([`BrowserEvent::DoneTasksDue`]).
    pub(crate) fn arm_done_tasks(&mut self, wait: u64, cx: &mut Context<Self>) {
        if self.task_done_timer.is_some() {
            return;
        }
        self.task_done_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(wait + 50)).await;
            let _ = this.update(cx, |this, cx| {
                this.task_done_timer = None;
                cx.emit(BrowserEvent::DoneTasksDue);
            });
        }));
    }
}
