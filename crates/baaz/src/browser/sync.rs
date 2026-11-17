//! The Browse reconciliation (task 4.5 split of `browser/mod.rs`): the centre
//! page's geometry and `sync_browser`, which runs from `render` and places,
//! shows, hides and discards the native pages, records history and drives
//! the adblock cosmetics. The second half of task 4.5 moves it behind
//! observation.

use super::*;

impl Harness {

    /// Whether the centre's tab itself is blank (its stored URL): a tab
    /// mid-way through its first navigation is not, even while its live
    /// page still reports `about:blank`.
    pub(crate) fn browse_centre_tab_blank(&self, cx: &gpui::App) -> bool {
        self.browser.browse_centre_shown(cx)
            .and_then(|id| self.browser.tabs.get(id))
            .is_none_or(|tab| tab.url.is_empty() || tab.url == BLANK)
    }

    /// The Browse centre's live page, when one already exists — the
    /// shown tab's state — never creating. The mouse-down hand-back
    /// reads this instead of the pane's page while in Browse.
    pub(crate) fn browse_centre_state(&self, cx: &gpui::App) -> Option<Entity<WebviewState>> {
        let id = self.browser.browse_centre_shown(cx)?;
        self.browser.states.get(&id).cloned()
    }

    /// The open terminal dock's height in window pixels, else zero
    /// (B2a-H2b): the one height behind the centre rect, the sync's
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
    /// release: `sync_browser` stores it every frame
    /// ([`BrowseState::centre_page_rect`]) and the mouse-down
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
            if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse && self.agent_window.card_ui == crate::app::agent_window::CardUi::Column {
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
        self.ensure_tabs_loaded();
        let id = self.browser.browse_centre_shown(cx)?;
        Some(self.ensure_tab_page(HOME_KEY, id, window, cx))
    }

    /// Drop hidden tabs' live pages. On the frame (`force` false) only
    /// tabs idle past the timeout go; `tab-discard-now` forces every hidden
    /// page at once. Audible pages, pinned tabs and the agent's active
    /// tabs are always spared; the shown tabs — the pane's and Browse's
    /// centre — are never candidates. Showing a dropped tab again
    /// rebuilds its page at its URL.
    pub(super) fn discard_hidden_tabs(
        &mut self,
        force: bool,
        shown: Option<TabId>,
        centre: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        let now = tabs::now_ms();
        let agent: std::collections::HashSet<TabId> =
            self.browser.agent_tabs.values().copied().collect();
        let live: Vec<(TabId, bool, bool)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| {
                let page = state.read(cx);
                let tab = self.browser.tabs.get(*id);
                (*id, page.audible(), tab.is_some_and(|tab| tab.pinned))
            })
            .collect();
        let mut dropped = Vec::new();
        for (id, audible, pinned) in live {
            if Some(id) == shown || Some(id) == centre || audible || pinned || agent.contains(&id) {
                continue;
            }
            let last = self.browser.tabs.get(id).and_then(|tab| tab.last_shown);
            // Forced (`tab-discard-now`) drops every hidden page at once;
            // the frame only drops tabs idle past the timeout.
            let due = if force {
                true
            } else {
                tabs::should_discard(now, last, audible, false)
            };
            if due {
                dropped.push(id);
            }
        }
        for id in &dropped {
            self.browser.states.remove(id);
            // The tools' registry would keep the discarded page alive (and
            // on screen, and uncollected) — see `close_tab`.
            self.terminal_service.unregister_tab_page(id.0);
        }
        // A discarded page can never stay registered with the agent tools:
        // agent tabs are spared above, so this fires only if that ever
        // stops holding — and then the registration dies with the page.
        let dropped: std::collections::HashSet<TabId> = dropped.into_iter().collect();
        let stale: Vec<String> = self
            .browser
            .agent_tabs
            .iter()
            .filter(|(_, id)| dropped.contains(id))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.browser.agent_tabs.remove(&key);
            self.terminal_service.unregister_browser(&key);
        }
    }

    /// Set every live page's obscured flag from the shown tabs: the
    /// pane's page shows while the frame is live, Browse's centre page
    /// while the centre is live, every other page hides. Blank pages stay
    /// hidden (WKWebView paints `about:blank` white over the dark pane).
    pub(super) fn apply_page_visibility(
        &mut self,
        shown: Option<TabId>,
        frame_live: bool,
        centre: Option<TabId>,
        centre_live: bool,
        cx: &mut Context<Self>,
    ) {
        // R4: the Split partner shows with the centre; under a Peek both
        // freeze into their stand-ins (one decision per page per frame —
        // flipping a page live and back each frame never lets it settle).
        let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let partner = if in_browse { self.split_partner(cx) } else { None };
        let peeking = in_browse && self.browser.peek.is_some();
        let pages: Vec<(TabId, Entity<WebviewState>)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| (*id, state.clone()))
            .collect();
        for (id, state) in pages {
            let blank = matches!(state.read(cx).url().as_ref(), "" | BLANK);
            let in_centre = Some(id) == centre || Some(id) == partner;
            let visible =
                (frame_live && Some(id) == shown || centre_live && in_centre && !peeking) && !blank;
            state.update(cx, |state, cx| state.set_obscured(!visible, cx));
        }
    }

    /// Per-frame browser sync, called from `render` after the overlay set is
    /// known: pull live pages into their tabs (persisting on change),
    /// persist real navigations onto their sessions, resolve pending
    /// screenshot attaches, discard long-hidden pages, and hide every
    /// native view that is not the shown tab. Called with the
    /// already-computed overlay elements (`None` is closed), so this reads
    /// state only.
    /// Whether the Browse centre may show its page this frame (B2a-H2b):
    /// the current project's person tab is there and nothing covers it —
    /// the Skills page and a pending provider switch hide it exactly
    /// like the palette, menus, dialogs and settings hide the pane's.
    pub(crate) fn browse_centre_live(&self, cx: &gpui::App) -> bool {
        self.browser.browse_centre_shown(cx).is_some()
            && self.pending_switch_dest(cx).is_none()
            && !crate::region::Region::covers_page(self.skills_page.read(cx), cx)
    }

    /// Ad blocking's element hiding (owner, 12 Nov: blocked ads left empty
    /// boxes): each loaded http(s) page is scanned for its classes and ids
    /// once it stops loading, and again 2.5 s later for slots filled in late;
    /// the matching rules go in as one stylesheet. Cheap per frame: a map
    /// look-up per live tab.
    pub(super) fn adblock_cosmetics(&mut self, live: &[(TabId, String, String, Option<String>)], cx: &mut Context<Self>) {
        const SECOND_PASS: std::time::Duration = std::time::Duration::from_millis(2_500);
        let now = std::time::Instant::now();
        self.browser.cosmetic_pass.retain(|id, _| live.iter().any(|(tab, ..)| tab == id));
        for (id, url, ..) in live {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                continue;
            }
            let Some(state) = self.browser.states.get(id).cloned() else { continue };
            if state.read(cx).loading() || self.browser.cosmetic_reads.values().any(|(tab, _)| tab == id) {
                continue;
            }
            let pass = match self.browser.cosmetic_pass.get(id) {
                Some((seen, n, _)) if seen == url && *n >= 2 => continue,
                Some((seen, 1, first)) if seen == url => {
                    if now.duration_since(*first) < SECOND_PASS {
                        continue;
                    }
                    2
                }
                Some((seen, ..)) if seen == url => continue,
                _ => 1,
            };
            let first = if pass == 1 { now } else { self.browser.cosmetic_pass.get(id).map_or(now, |p| p.2) };
            self.browser.cosmetic_pass.insert(*id, (url.clone(), pass, first));
            let request = self.browser.next_text_read;
            self.browser.next_text_read += 1;
            self.browser.cosmetic_reads.insert(request, (*id, url.clone()));
            state.update(cx, |state, _| state.eval_with_result(request, adblock::COSMETIC_SCAN_JS));
            // A quiet window takes no frame: wake for the answer, and once
            // more for the second pass.
            let waits: &[u64] = if pass == 1 { &[300, 1200, 2_700] } else { &[300, 1200] };
            let waits = waits.to_vec();
            cx.spawn(async move |this, cx| {
                for wait in waits {
                    cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
            .detach();
        }
    }

    /// R4 history (docs/27 §7): every person tab's page becomes a visit, a
    /// late title updates it, and the page in front gives up its words once
    /// it has stayed [`history::DWELL_MS`]. A session's tabs never record.
    /// Runs from the sync, so it must stay cheap: map lookups, and a write
    /// queued to the `browser.db` worker ([`super::data`]) only when a page
    /// or title changed — never a SQLite call here.
    pub(super) fn record_history(&mut self, live: &[(TabId, String, String, Option<String>)], cx: &mut Context<Self>) {
        let now = tabs::now_ms();
        let front = self.browser.last_centre_shown;
        let mut changed = false;
        let mut want: Vec<(TabId, String, String)> = Vec::new();
        let writer = self.browser.data.read(cx).writer();
        for (id, url, title, _) in live {
            let person = matches!(self.browser.tabs.get(*id).map(|tab| &tab.owner), Some(Owner::Person));
            let before = self.browser.recorder.url_of(*id).map(str::to_owned);
            let note = self.browser.recorder.observe(&writer, *id, person, url, title, front == Some(*id), now);
            // Only a page history keeps is a change: an unrecordable page
            // (a blank tab) is forgotten by the recorder every frame, and
            // must not ask the Field's history again each time (its answer
            // lands and wakes the window: task 4.5).
            if before.as_deref() != Some(url.as_str()) && person && history::recordable(url) {
                changed = true;
            }
            if note == history::Note::WantText {
                want.push((*id, url.clone(), title.clone()));
            }
        }
        if changed {
            self.browser.data.read(cx).history_changed();
        }
        self.browser.recorder.retain(|id| live.iter().any(|(tab, ..)| *tab == id));
        for (id, url, title) in want {
            let Some(state) = self.browser.states.get(&id).cloned() else { continue };
            let request = self.browser.next_text_read;
            self.browser.next_text_read += 1;
            crate::baaz_log!("history: reading the words of {url}");
            self.browser.text_reads.insert(request, (id, url, title));
            state.update(cx, |state, _| state.eval_with_result(request, history::PAGE_TEXT_JS));
            // The answer lands on the page's poll, which takes no frame on
            // its own in a quiet window: wake twice to collect it while the
            // tab is still on that page.
            cx.spawn(async move |this, cx| {
                for wait in [300u64, 1200] {
                    cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
            .detach();
        }
        self.adblock_cosmetics(live, cx);
        // Answers: keep the words if the tab is still on that page. One
        // drain serves every script Baaz runs here (page words, ad-box
        // scans, stylesheet installs): taking a page's answers takes all of
        // them, so each is routed by its request id, never dropped.
        let pending = !self.browser.text_reads.is_empty()
            || !self.browser.cosmetic_reads.is_empty()
            || !self.browser.cosmetic_injects.is_empty();
        if pending {
            let states: Vec<(TabId, Entity<WebviewState>)> =
                self.browser.states.iter().map(|(id, state)| (*id, state.clone())).collect();
            for (id, state) in states {
                let waiting = self.browser.text_reads.values().any(|(tab, _, _)| *tab == id)
                    || self.browser.cosmetic_reads.values().any(|(tab, _)| *tab == id)
                    || !self.browser.cosmetic_injects.is_empty();
                if !waiting {
                    continue;
                }
                let answers = state.update(cx, |state, _| state.take_eval_results());
                for (request, answer) in answers {
                    if self.browser.cosmetic_injects.remove(&request) {
                        continue;
                    }
                    if let Some((tab, url)) = self.browser.cosmetic_reads.remove(&request) {
                        if tab == id {
                            self.browser.land_cosmetic_scan(&state, &url, answer, cx);
                        }
                        continue;
                    }
                    let Some((tab, url, title)) = self.browser.text_reads.remove(&request) else { continue };
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
                        self.browser.data.update(cx, |data, _| data.set_text(&url, &title, &text));
                    }
                }
            }
        }
        // A quiet window takes no frame: wake once when the page in front
        // has stayed long enough, so its words are kept without input.
        if let Some(id) = front {
            let url = live.iter().find(|(tab, ..)| *tab == id).map(|(_, url, ..)| url.clone());
            if let Some(url) = url.filter(|url| history::recordable(url)) {
                if self.browser.dwell_armed.as_ref() != Some(&(id, url.clone())) {
                    self.browser.dwell_armed = Some((id, url));
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(history::DWELL_MS + 250))
                            .await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                }
            }
        }
    }

    pub(crate) fn sync_browser(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded();
        // Live pages report into their tabs — debounced to actual changes,
        // never one write per poll.
        let live: Vec<(TabId, String, String, Option<String>)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| {
                let page = state.read(cx);
                (*id, page.url().to_string(), page.title().to_string(), page.favicon().map(|url| url.to_string()))
            })
            .collect();
        let mut tabs_dirty = false;
        for (id, url, title, favicon) in &live {
            if self.browser.tabs.set_url_title(*id, url, title) {
                tabs_dirty = true;
            }
            if let Some(favicon) = favicon {
                if self.browser.tabs.set_favicon(*id, favicon) {
                    tabs_dirty = true;
                }
            }
        }
        self.fetch_favicons(cx);
        // The session's active tab keeps writing `browser_url`, so older
        // builds still restore — debounced to actual changes, never one
        // write per poll. B7: a navigation is not searchable, so it
        // persists through the cheap debounced write — never the
        // session-list settle.
        let urls: HashMap<TabId, String> =
            live.iter().map(|(id, url, _, _)| (*id, url.clone())).collect();
        let actives: Vec<(Owner, String)> = self
            .browser
            .tabs
            .active
            .values()
            .filter_map(|id| {
                let tab = self.browser.tabs.get(*id)?;
                let url = urls.get(id)?.clone();
                Some((tab.owner.clone(), url))
            })
            .collect();
        for (owner, current) in actives {
            if let Owner::Session(session) = owner {
                let stored = self
                    .overrides
                    .get(&session)
                    .and_then(|meta| meta.right.clone())
                    .and_then(|right| right.browser_url);
                if should_remember(&current, stored.as_deref()) {
                    self.remember_browser_url_cheap(&session, current, cx);
                }
            }
        }
        if tabs_dirty {
            self.browser.schedule_tabs_write();
        }
        self.record_history(&live, cx);
        // The header toolbar's back/forward stacks follow every live page:
        // whatever the page reports becomes the stack's current URL, so an
        // outside navigation (a link) lands here and clears the forward
        // stack exactly like a fresh one. Whether a load runs is the
        // engine's own `WebviewState::loading`, read where the stop/reload
        // control needs it — never tracked here.
        for (id, url, _, _) in &live {
            self.browser.histories.entry(*id).or_default().note_navigated(url);
        }
        // B2a-H5, before the shown-tab computation below: popups become
        // tabs (foreground ones select, so the centre follows),
        // downloads stamp their start for the popover's estimates, and
        // closed tabs drop their find strips.
        let _ = self.drain_browse_popups(cx);
        self.drain_link_gestures(window, cx);
        self.browser.prune_split();
        self.browser.stamp_browse_downloads(cx);
        let live: HashSet<TabId> = self.browser.tabs.tabs.iter().map(|tab| tab.id).collect();
        self.browser.find_strips.retain(&live);
        // A closed tab drops its strip above: with none standing open the
        // shared input has no reader, so it drops too — the same hand-over
        // `close_browse_find` does for Done/Esc.
        if !self.browser.find_strips.any_open() {
            self.browser.browse_find = None;
            self.browser.browse_find_sub = None;
        }
        // A Screenshot whose bytes were not there yet attaches on arrival.
        // Pending captures key to the tab they were taken on
        // ([`screenshot_tab_key`]): the bytes attach to the active
        // session's draft only while that tab still shows, so a switch
        // can never misattribute them.
        let active_key = self.browser_key(cx);
        let shown_now = self.browser.browse_centre_shown(cx);
        let pending: Vec<String> = self.browser.shot_pending.iter().cloned().collect();
        for key in pending {
            if let Some(id) = screenshot_tab_id(&key) {
                let ready = self
                    .browser
                    .states
                    .get(&id)
                    .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec));
                if let Some(bytes) = ready {
                    if shown_now == Some(id) {
                        self.browser.shot_pending.remove(&key);
                        self.attach_browser_screenshot(bytes, cx);
                    }
                }
                continue;
            }
            let owner = self.browser.tab_owner_for_key(&key);
            let space = self.browser.owner_space(&owner);
            let bytes = self
                .browser
                .tabs
                .active_of(&owner, space.as_deref())
                .and_then(|id| self.browser.states.get(&id))
                .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec));
            if let Some(bytes) = bytes {
                if key == active_key {
                    self.browser.shot_pending.remove(&key);
                    self.attach_browser_screenshot(bytes, cx);
                }
            }
        }
        // The native-overlay rule: every webview hides unless the frame
        // shows its session's Browser kind with nothing over it.
        let overlays = self.overlays.read(cx);
        let menu_kind = overlays.menu.as_ref().map(|menu| menu.kind);
        // The Settings page covers the page while it stands (the region
        // contract's `covers_page`), as the two sheets do.
        let settings_open = crate::region::Region::covers_page(self.settings_page.read(cx), cx)
            || self.muse_sheet
            || self.setup_open(cx);
        // A modal the overlay window hosts sits above the live page: it
        // covers nothing (`OverlayKind::Modal`).
        let modal_hosted = self.modal_overlay_wanted(cx);
        let palette_open = overlays.palette.is_some() && !modal_hosted;
        // Every menu on the stack draws in the main window, so it covers the
        // page; the account menu stays inside the sidebar column. Browse's
        // right-click menus live apart (`browse_menu`), in an overlay window.
        let menu_open = menu_kind.is_some_and(|kind| kind != MenuKind::Account);
        let dialog_open = overlays.dialog.is_some() && !modal_hosted;
        // The composer's `+` menu is plain view state, never the overlay
        // stack — read it off the active view. Audit: caret popovers and
        // chip pickers ride `overlays.menu` above; the row-detail card hangs
        // beside the sidebar, tooltips seat on the header, and the
        // queue/steer strips render inline in the composer — none of them
        // covers the right pane, so the `+` menu is the only extra input.
        let plus_open = self
            .active
            .as_ref()
            .is_some_and(|view| view.read(cx).plus_open());
        let kind = crate::layout::right_kind(crate::layout::Layout::get(cx));
        let content = window_content_bounds(window);
        // Q4c: while a provider switch is pending no session's page shows —
        // `browser_key` still names the previous session, and its webview
        // must neither draw nor hit-test under the loading centre.
        let pending_switch = self.pending_right_neutral(cx);
        // The native-overlay rule: every page hides unless the frame shows
        // its owner's Browser kind with nothing over it — and only the
        // shown tab's page ever shows. A tab is its owner's page exactly
        // when its owner is the frame's owner.
        let shown_owner = self.browser.tab_owner_for_key(&active_key);
        let shown_key = match &shown_owner {
            Owner::Session(session) => session.clone(),
            Owner::Person => HOME_KEY.to_owned(),
        };
        // Only its owner's page ever shows — and, while a provider switch
        // is pending, not even that one.
        let shown_space = self.browser.owner_space(&shown_owner);
        let shown = browser_session_active(&shown_key, &active_key, pending_switch)
            .then(|| {
                self.browser
                    .tabs
                    .active_of(&shown_owner, shown_space.as_deref())
            })
            .flatten();
        // Who holds the keyboard, once per frame: both the pane's and the
        // centre's liveness read it, and only the really shown page ever
        // counts (see below).
        let holds: HashMap<TabId, bool> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| (*id, state.read(cx).holds_keyboard()))
            .collect();
        // The Baaz-drawn crashed page covers the centre page's rect. The
        // chrome popovers are resolved below, once the centre tab is known.
        let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let page_error_open = self
            .browser.browse_centre_shown(cx)
            .and_then(|id| self.browser.states.get(&id))
            .is_some_and(|state| state.read(cx).crashed().is_some());
        let base = BrowserVisibility {
            pane_open: crate::layout::Layout::get(cx).right_open,
            kind_browser: kind == RightKind::Browser,
            centre_browse: false,
            session_active: false,
            settings_open,
            palette_open,
            menu_open,
            plus_open,
            dialog_open,
            drop_cover: self.browser.drop_cover,
            // F2 Active only: Paused leaves the app usable, so its pages
            // stay live.
            feedback_cover: self.feedback.covers_app(),
            // The Field draws its rows in the overlay window, over the live
            // page: only the in-window dropdowns still cover it.
            omnibox_open: self.browse_omnibox_open(cx) && !self.field_overlay_wanted(cx),
            chrome_popover_open: false,
            page_error_open,
            // No "window active" input: the page is a child window of
            // ours, ordered with it, so another window taking the keyboard
            // (a menu-bar menu, DevTools, the page itself) covers nothing.
            // Hiding on deactivation blanked the page under every menu-bar
            // click and the Inspect window (owner screenshots 2026-10-11).
        };
        // The frame shows a page exactly when the pane stands open on the
        // Browser kind with nothing over it; ownership rides `shown`, which
        // is the frame owner's active tab (or nothing while pending).
        // D6 (docs/43 §3): while the pane's strip slides, the Page screen
        // is a still picture (the cover technique) — the live page window
        // shows again once the slide lands.
        let frame_live = browser_visible(BrowserVisibility {
            session_active: true,
            ..base
        // D6: while the strip slides, and where the Page screen draws
        // Baaz's own state (no page yet, couldn't load, a pushed view), the
        // pane covers its page (the region contract's `covers_page`).
        }) && !crate::region::Region::covers_page(self.right_pane.read(cx), cx);
        // B2a-H2b: in Browse the pane never shows a page — one tab, one
        // page — so the session page can neither share the centre's tab
        // nor paint behind the mode. The render path shows the Changes
        // fallback instead, and the sync hides every pane page here.
        let (shown, frame_live) = if in_browse { (None, false) } else { (shown, frame_live) };
        // B2a-H2: in Browse the centre shows its tab's page under what the
        // main window draws over it (see `centre_visibility`); the Skills
        // page and a pending provider switch hide it too. The centre rect is
        // stored for the mouse-down hand-back: the single source behind
        // placement and release.
        let centre_shown = self.browser.browse_centre_shown(cx);
        // B2a-H5: a switch dismisses the chrome popovers (they point at
        // the old tab's state), and leaving Browse closes them all; the
        // find strip is per tab and stays. The last-seen tab stores every
        // frame — including from `None` — so the switch check is never
        // dead and no flag leaks into Sessions mode. The first sight sets
        // without dismissing, so a `--steps` popover opens onto its own
        // first frame.
        let switched = self.browser.last_centre_shown.is_some() && self.browser.last_centre_shown != centre_shown;
        if switched {
            // Every route to a new centre tab (close, ⌘T, popup, omnibox,
            // task folder) — not only a sidebar click — moves find: the
            // tab left stops highlighting, and the shared input picks up
            // the new tab's query on the next frame (`on_frame` holds the
            // mutable window that `set_value` needs).
            self.browser.find_resync = Some((self.browser.last_centre_shown, centre_shown));
        }
        if switched || !in_browse {
            self.browser.downloads_open = false;
            self.browser.extensions_open = false;
            self.browser.downloads_rendered = false;
            self.browser.more_open = false;
        }
        self.browser.last_centre_shown = centre_shown;
        // B2a-H5: covering is honest — a popover covers the page only while
        // it actually renders. A flag whose popover cannot render (no
        // downloads, no centre state) clears instead of blanking the page
        // under an invisible popover; the downloads flag clears only once
        // it rendered, so a scripted download's first frame never closes
        // its own capture.
        let centre_has_state = centre_shown.is_some_and(|id| self.browser.states.contains_key(&id));
        let downloads_empty = self.browser.browse_downloads(cx).is_empty();
        if self.browser.downloads_open && centre_shown.is_some() && !downloads_empty {
            self.browser.downloads_rendered = true;
        }
        // The footer's popover (anchored) shows an empty list as such.
        if self.browser.downloads_open
            && self.browser.downloads_anchor.is_none()
            && chrome::downloads_flag_clears(centre_shown.is_some(), downloads_empty, self.browser.downloads_rendered)
        {
            self.browser.downloads_open = false;
            self.browser.downloads_rendered = false;
        }
        if self.browser.extensions_open && centre_shown.is_none() {
            self.browser.extensions_open = false;
        }
        if self.browser.more_open && !centre_has_state {
            self.browser.more_open = false;
        }
        let chrome_popover_open = in_browse
            && ((self.browser.downloads_open && !self.browser.downloads_overlay_wanted(cx) && centre_shown.is_some() && !self.browser.browse_downloads(cx).is_empty())
                || (self.browser.extensions_open && centre_shown.is_some() && !self.browser.downloads_overlay_wanted(cx))
                || (self.browser.more_open && centre_has_state));
        // A crashed page hides the find strip: the highlight stops and the
        // strip closes, so the error page owns the rect alone. First, so
        // the stored rect below never reserves a row for a strip that just
        // closed.
        if let Some(id) = centre_shown {
            if let Some(state) = self.browser.states.get(&id).cloned() {
                if state.read(cx).crashed().is_some() && self.browser.find_strips.for_tab(id).close() {
                    state.update(cx, |state, _| state.stop_find());
                }
            }
        }
        // The find and permission strips are in-flow rows above the page,
        // so the stored page rect starts below them: placement and the
        // mouse-down release share this one rect.
        let find_open = self.browser.find_strips.visible(centre_shown).is_some();
        let permission_open = centre_shown
            .and_then(|id| self.browser.states.get(&id))
            .is_some_and(|state| !state.read(cx).permissions().is_empty());
        let stack = chrome::strip_stack_px(find_open, permission_open);
        let mut centre_page = self.browse_centre_page_rect(content, window, cx);
        if in_browse && stack > 0.0 {
            centre_page.origin.y += gpui::px(stack);
            centre_page.size.height = (centre_page.size.height - gpui::px(stack)).max(gpui::px(0.0));
        }
        self.browser.centre_page_rect = in_browse.then_some(centre_page);
        // Tabs that navigated away (or closed) drop their "while visiting"
        // grants; then trusted origins auto-answer and background asks
        // mark their rows while in Browse.
        self.browser.revoke_stale_visit_grants();
        self.browser.route_browse_permissions(centre_shown, in_browse, cx);
        let centre_live = browser_visible(centre_visibility(
            base,
            centre_shown.is_some(),
            self.browse_centre_live(cx),
            chrome_popover_open,
        ));
        // Editing ends when the page takes the keyboard (B2a-H4 7b): a click
        // into a Chromium page moves no gpui focus, so the address input
        // never blurs and would keep its editing look. The per-frame sync
        // ends it instead — on the rising edge only, so typing in the field
        // (which the page never holds through) never cancels itself.
        // Only a page the person can see and click counts: creating a tab's
        // (hidden, blank) Chromium window makes it key for a moment, which
        // dropped the new-tab field ⌘T had just focused (B2a live pass).
        let centre_blank = centre_shown
            .and_then(|id| self.browser.tabs.get(id))
            .is_none_or(|tab| tab.url.is_empty() || tab.url == BLANK);
        let centre_holds = centre_live
            && !centre_blank
            && centre_shown.and_then(|id| holds.get(&id).copied()).unwrap_or(false);
        if in_browse && centre_holds && !self.browser.browse_last_holds {
            // The page just took the keyboard: drop the field with no
            // refocus (the sync owns no `&mut Window`, and handing focus
            // anywhere would steal it straight back from the page).
            self.drop_browse_address(cx);
        }
        self.browser.browse_last_holds = centre_holds;
        self.apply_page_visibility(shown, frame_live, centre_shown, centre_live, cx);
        // R4: a Split partner shows beside the centre; a Peek shows over it,
        // the centre (and its partner) frozen into their stand-ins beneath.
        let partner = if in_browse { self.split_partner(cx) } else { None };
        let peeking = in_browse && self.browser.peek.is_some();
        // A click into the other half (it just took the keyboard) makes it
        // the shown tab: the rising edge, since the left page may still
        // report holding it.
        let partner_holds = partner.is_some_and(|p| holds.get(&p).copied().unwrap_or(false));
        // The page script reports a press (`take_clicked`): read both
        // halves every frame so an old press never counts later.
        let centre_clicked = centre_shown
            .and_then(|id| self.browser.states.get(&id).cloned())
            .is_some_and(|state| state.update(cx, |state, _| state.take_clicked()));
        let partner_clicked = partner
            .and_then(|id| self.browser.states.get(&id).cloned())
            .is_some_and(|state| state.update(cx, |state, _| state.take_clicked()));
        if let Some(partner) = partner {
            let rising = partner_holds && !self.browser.partner_held;
            if centre_live && !peeking && (partner_clicked && !centre_clicked || rising) {
                crate::baaz_log!("browse: split focus → tab {}", partner.0);
                self.browser.split_focus = Some(partner);
            }
        }
        self.browser.partner_held = partner_holds;
        if let Some(peek) = self.browser.peek.as_ref() {
            let state = peek.state.clone();
            state.update(cx, |state, cx| state.set_obscured(!(peeking && centre_live), cx));
        }
        if !in_browse && self.browser.peek.is_some() {
            self.close_peek(cx);
        }
        // The shown pages stamp their tabs when they really show (never a
        // blank), so discard measures real hiding — in memory only, never
        // a write on its own.
        for (id, live) in [(shown, frame_live), (centre_shown, centre_live)] {
            if live {
                if let Some(id) = id {
                    let blank = self
                        .browser
                        .states
                        .get(&id)
                        .map(|state| matches!(state.read(cx).url().as_ref(), "" | BLANK))
                        .unwrap_or(true);
                    if !blank {
                        self.browser.tabs.mark_shown(id, tabs::now_ms());
                    }
                }
            }
        }
        // Long-hidden pages drop their native views (but keep their tabs);
        // the shown tabs are never candidates.
        self.discard_hidden_tabs(false, shown, centre_shown, cx);
        // Task folders done for 15 minutes close their tabs (R3).
        if self.browser.tabs_loaded {
            self.close_done_task_tabs(centre_shown.or(shown), window, cx);
        }
        if tabs_dirty {
            self.browser.schedule_tabs_write();
        }
    }
}

impl BrowseState {

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


    /// Whether a page that was just covered is still live, waiting for the
    /// picture that stands in for it ([`WebviewState::cover_settling`]).
    /// The frame holds its covering UI back until it is not: gpui cannot
    /// paint over the live page, so a cover drawn sooner showed only its
    /// part over the gpui ground — the ⌘K scrim dimmed the sidebar ~165 ms
    /// before the page (owner recording 2026-10-11).
    pub(crate) fn browser_cover_settling(&self, cx: &gpui::App) -> bool {
        self.states.values().any(|state| state.read(cx).cover_settling())
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
}
