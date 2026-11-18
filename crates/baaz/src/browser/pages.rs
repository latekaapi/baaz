//! The browser's pages (task 4.5 split of `browser/mod.rs`): building or
//! reusing a tab's webview, prewarm, opening a link in Browse, the session
//! pages, popups and the page intents (screenshot, annotate, draft).

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    /// The webview for `key`, creating it on first use. `key` names an
    /// owner (a session id, or [`HOME_KEY`] for the person), and the page
    /// is that owner's active tab's — opening the tab first when the owner
    /// has none. A new page opens on the tab's URL; only an explicit
    /// person open (`focus_fresh`) takes the keyboard into a fresh blank
    /// URL field, so ⌘L is already where the person looks. Activation,
    /// restore and `browse:` steps pass `false`: the composer keeps focus.
    pub(super) fn browser_for(
        &mut self,
        key: &str,
        focus_fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        self.ensure_tabs_loaded(cx);
        let owner = self.browser.read(cx).tab_owner_for_key(key);
        let id = self.ensure_owner_tab(&owner, cx);
        if let Some(state) = self.browser.read(cx).states.get(&id).cloned() {
            self.register_tab_page(key, id, &state, cx);
            return state;
        }
        let url = self
            .browser
            .read(cx)
            .tabs
            .get(id)
            .map(|tab| tab.url.clone())
            .unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
        let fresh_blank = initial == BLANK;
        // Explicit boot flag, never environment sniffed in render: captures
        // and tests run the scripted page, the app runs WKWebView.
        self.browser.update(cx, |browser, _| browser.browser_builds += 1);
        let fake = self.browser.read(cx).fake || cfg!(test);
        let profile = self.browser.read(cx).tab_profile(id);
        let state = cx.new(|cx| {
            if fake {
                let mut backend = FakeWebBackend::new();
                if backend.url().as_ref() != initial {
                    backend.navigate(initial);
                }
                WebviewState::new(Box::new(backend), cx)
            } else {
                // WebKit paints white before and beneath a page; paint the
                // pane's own surface instead, so about:blank is not a white slab.
                let bg = gpui::Rgba::from(cx.aui().colors.surface_1);
                let rgba =
                    [bg.r, bg.g, bg.b, bg.a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                let rgba = (rgba[0], rgba[1], rgba[2], rgba[3]);
                if let Some(backend) = cef_backend(&*window, initial, rgba, profile.as_deref()) {
                    return WebviewState::new(backend, cx);
                }
                match WryBackend::new_at_with_background(
                    &*window,
                    initial,
                    (0.0, 0.0),
                    (0.0, 0.0),
                    rgba,
                ) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!(
                            "browser: no WKWebView ({error}); falling back to the scripted page"
                        );
                        let mut backend = FakeWebBackend::new();
                        if backend.url().as_ref() != initial {
                            backend.navigate(initial);
                        }
                        WebviewState::new(Box::new(backend), cx)
                    }
                }
            }
        });
        if focus_fresh && fresh_blank {
            let focus = state.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        self.browser.update(cx, |browser, cx| browser.adopt_page(id, state.clone(), cx));
        // The agent's `browser_*` tools act on this same state through the
        // socket service, keyed by session id: always the session's active
        // tab's page.
        self.register_tab_page(key, id, &state, cx);
        state
    }

    /// The live webview the pane shows, when one already exists — the
    /// shown owner's active tab's page — never creating. The closing frame
    /// uses this: the page's last snapshot stays on screen while the column
    /// slides out instead of the "Opening the page" placeholder (B7).
    pub(crate) fn existing_browser(&self, cx: &gpui::App) -> Option<Entity<WebviewState>> {
        if !self.browser.read(cx).tabs_loaded {
            return None;
        }
        let owner = self.shown_tab_owner(cx);
        let space = self.browser.read(cx).owner_space(&owner);
        self.browser
            .read(cx)
            .tabs
            .active_of(&owner, space.as_deref())
            .and_then(|id| self.browser.read(cx).states.get(&id).cloned())
    }


    /// Prepare the shown tab's webview ahead of the pane animating
    /// (B7): `toggle_right`/`show_right` call this when they land open on
    /// Browser, so the opening frame shows the page instead of paying the
    /// construction cost inside `render`. Only the scripted backend can be
    /// prepared without a window — see [`Self::prewarm_browser_in`] for the
    /// real webview, which the windowed open paths build instead. Never
    /// focuses: focus rides only on the person's own open through
    /// [`Self::ensure_browser_person`].
    pub(crate) fn prewarm_browser_if_needed(&mut self, cx: &mut Context<Self>) {
        // B2a-H2b: in Browse the pane never shows a page, so nothing
        // prewarms one for it — the centre builds its own on show.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            return;
        }
        if !crate::layout::Layout::get(cx).right_open || crate::layout::right_kind(crate::layout::Layout::get(cx)) != RightKind::Browser
        {
            return;
        }
        if !(self.browser.read(cx).fake || cfg!(test)) {
            return;
        }
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let id = self.ensure_owner_tab(&owner, cx);
        if self.browser.read(cx).states.contains_key(&id) {
            return;
        }
        let url = self
            .browser
            .read(cx)
            .tabs
            .get(id)
            .map(|tab| tab.url.clone())
            .unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
        self.browser.update(cx, |browser, _| browser.browser_builds += 1);
        let state = cx.new(|cx| {
            let mut backend = FakeWebBackend::new();
            if backend.url().as_ref() != initial {
                backend.navigate(initial);
            }
            WebviewState::new(Box::new(backend), cx)
        });
        self.browser.update(cx, |browser, cx| browser.adopt_page(id, state.clone(), cx));
        let key = self.browser_key(cx);
        self.register_tab_page(&key, id, &state, cx);
    }

    /// Build the real webview outside `render` (B7fix2): the open action's
    /// own construction, with the window in hand — the shipped app's
    /// WKWebView, the scripted page in tests and captures. Callers are the
    /// click/key handlers that land open on Browser, the deferred warmup
    /// behind [`crate::app::Harness::defer_browser_prewarm`], and the first
    /// signed-in frame. Never more than one webview per session (an
    /// existing one is reused), never focused: focus rides only on the
    /// person's own open through [`Self::ensure_browser_person`].
    pub(crate) fn prewarm_browser_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // B2a-H2b: like `prewarm_browser_if_needed` — no pane page in
        // Browse, so no prewarm for one.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            return;
        }
        if !crate::layout::Layout::get(cx).right_open || crate::layout::right_kind(crate::layout::Layout::get(cx)) != RightKind::Browser
        {
            return;
        }
        let key = self.browser_key(cx);
        let owner = self.browser.read(cx).tab_owner_for_key(&key);
        let exists = self.browser.read(cx).tabs_loaded
            && self
                .browser
                .read(cx)
                .tabs
                .active_of(&owner, self.browser.read(cx).owner_space(&owner).as_deref())
                .is_some_and(|id| self.browser.read(cx).states.contains_key(&id));
        if exists {
            return;
        }
        let _ = self.browser_for(&key, false, window, cx);
    }

    /// The webview the pane shows this frame, creating it on first use.
    /// The caller gates on pane-open-on-Browser, so activation, boot and
    /// every other kind never create a webview as a side effect; the
    /// `browse:` step and agent tools create through [`Self::browser_for`]
    /// directly instead. Focus rides only on the person's own open: the
    /// root's settle (else the pane's compose) consumes one armed
    /// `show_right`/`toggle_right` onto a blank page, and a restore leaves
    /// the composer's keyboard alone.
    /// Open a link's URL in Baaz's Browser pane (B9): the non-toggling
    /// open onto Browser, then the navigation. The scripted backend needs
    /// no window, so it navigates at once; the real webview navigates on
    /// the next render, when [`Self::ensure_browser_person`] consumes the
    /// pending URL with the window at hand. Never arms URL-field focus: a
    /// link is not the person's own open.
    pub(crate) fn open_url_in_browser(&mut self, url: String, cx: &mut Context<Self>) {
        self.open_right_on(RightKind::Browser, cx);
        // A person's own open earns URL-field focus; a link never does —
        // undo the arm `open_right_on` owes the person.
        self.browser.update(cx, |browser, _| browser.browser_url_focus_armed = false);
        if self.browser.read(cx).fake || cfg!(test) {
            self.ensure_tabs_loaded(cx);
            let owner = self.shown_tab_owner(cx);
            let id = self.ensure_owner_tab(&owner, cx);
            let state = self.browser.read(cx).states.get(&id).cloned().or_else(|| {
                self.prewarm_browser_if_needed(cx);
                self.browser.read(cx).states.get(&id).cloned()
            });
            if let Some(state) = state {
                if state.read(cx).url().as_ref() != url.as_str() {
                    let title = state.read(cx).title().to_string();
                    state.update(cx, |state, _cx| state.navigate(&url));
                    if self.browser.update(cx, |browser, _| browser.tabs.set_url_title(id, &url, &title)) {
                        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                    }
                }
            } else {
                self.browser.update(cx, |browser, _| browser.browser_pending_url = Some(url));
            }
        } else {
            self.browser.update(cx, |browser, _| browser.browser_pending_url = Some(url));
        }
        cx.notify();
    }

    pub(crate) fn ensure_browser_person(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        let focus = self.browser.update(cx, |browser, _| std::mem::take(&mut browser.browser_url_focus_armed));
        let key = self.browser_key(cx);
        // B7fix: the toggle prewarms the webview before the frame, so the
        // creation-time focus inside `browser_for` no longer fires on a
        // by-hand open — the state already exists by render. Honor the armed
        // focus on the prewarmed blank page here instead. Restores never arm
        // it, so they still leave the composer's keyboard alone, and an
        // armed open onto a navigated page still does not steal focus.
        let owner = self.browser.read(cx).tab_owner_for_key(&key);
        let existed = self.browser.read(cx).tabs_loaded
            && self
                .browser
                .read(cx)
                .tabs
                .active_of(&owner, self.browser.read(cx).owner_space(&owner).as_deref())
                .is_some_and(|id| self.browser.read(cx).states.contains_key(&id));
        let state = self.browser_for(&key, focus, window, cx);
        if focus && existed && state.read(cx).url() == BLANK {
            let focus_handle = state.read(cx).focus_handle().clone();
            window.focus(&focus_handle, cx);
        }
        // A link's pending URL lands here on the next render (B9): the
        // event had no window, so the navigation waited for one. Consumed
        // once — later frames reuse the navigated page.
        if let Some(url) = self.browser.update(cx, |browser, _| browser.browser_pending_url.take()) {
            if state.read(cx).url().as_ref() != url.as_str() {
                state.update(cx, |state, _cx| state.navigate(&url));
            }
        }
        state
    }

    /// Build (or reuse) the live page for `id` and register it under `key`.
    /// Showing a discarded tab again recreates its page at its URL.
    pub(crate) fn ensure_tab_page(
        &mut self,
        key: &str,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        if let Some(state) = self.browser.read(cx).states.get(&id).cloned() {
            self.register_tab_page(key, id, &state, cx);
            return state;
        }
        let url = self
            .browser
            .read(cx)
            .tabs
            .get(id)
            .map(|tab| tab.url.clone())
            .unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
        self.browser.update(cx, |browser, _| browser.browser_builds += 1);
        let fake = self.browser.read(cx).fake || cfg!(test);
        // The `--steps` capture seam: queued scripts land on the fresh
        // scripted backend before it is boxed, then drain through the real
        // poll path. Production never queues, so this takes nothing.
        let scripts = self.browser.update(cx, |browser, _| std::mem::take(&mut browser.fake_scripts));
        let profile = self.browser.read(cx).tab_profile(id);
        let state = cx.new(|cx| {
            if fake {
                let mut backend = FakeWebBackend::new();
                if backend.url().as_ref() != initial {
                    backend.navigate(initial);
                }
                for script in &scripts {
                    script.apply_to_fake(&mut backend);
                }
                WebviewState::new(Box::new(backend), cx)
            } else {
                let bg = gpui::Rgba::from(cx.aui().colors.surface_1);
                let rgba =
                    [bg.r, bg.g, bg.b, bg.a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                let rgba = (rgba[0], rgba[1], rgba[2], rgba[3]);
                if let Some(backend) = cef_backend(&*window, initial, rgba, profile.as_deref()) {
                    return WebviewState::new(backend, cx);
                }
                match WryBackend::new_at_with_background(
                    &*window,
                    initial,
                    (0.0, 0.0),
                    (0.0, 0.0),
                    rgba,
                ) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!(
                            "browser: no WKWebView ({error}); falling back to the scripted page"
                        );
                        let mut backend = FakeWebBackend::new();
                        if backend.url().as_ref() != initial {
                            backend.navigate(initial);
                        }
                        WebviewState::new(Box::new(backend), cx)
                    }
                }
            }
        });
        self.browser.update(cx, |browser, cx| browser.adopt_page(id, state.clone(), cx));
        self.register_tab_page(key, id, &state, cx);
        state
    }

    /// Attach `bytes` (a page screenshot) to the active session's composer
    /// draft as an image chip, keeping a durable copy under temp/attachments.
    /// No active session: a toast, never a lossy drop into the void.
    pub(super) fn attach_browser_screenshot(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.attach_browser_shot_named("browser-screenshot", bytes, cx);
    }

    /// [`Self::attach_browser_screenshot`] with the chip's name.
    pub(super) fn attach_browser_shot_named(&mut self, name: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let _path = write_screenshot_temp(&bytes);
        match self.active.clone() {
            Some(view) => {
                view.update(cx, |view, cx| {
                    view.attach_screenshot(name.to_owned(), bytes, cx);
                });
                cx.notify();
            }
            None => {
                self.overlays.update(cx, |overlays, cx| {
                    overlays.toast("Browser", "No open session to attach the screenshot to.", cx);
                });
            }
        }
    }

    /// One [`WebviewIntent`]: what the pane asks the app to do. Screenshots
    /// and annotations land in the active session's composer draft as
    /// attachments and text; nothing here ever auto-sends.
    pub(crate) fn handle_browser_intent(
        &mut self,
        intent: WebviewIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            WebviewIntent::FocusUrl => {
                let key = self.browser_key(cx);
                let state = self.browser_for(&key, false, window, cx);
                let focus = state.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            WebviewIntent::Console => {
                self.overlays.update(cx, |overlays, cx| {
                    overlays.toast("Browser", "Console is not available yet.", cx);
                });
            }
            WebviewIntent::Screenshot => {
                let key = self.browser_key(cx);
                let state = self.browser_for(&key, false, window, cx);
                state.update(cx, |state, _cx| state.capture());
                match state.read(cx).screenshot().map(<[u8]>::to_vec) {
                    Some(bytes) => self.attach_browser_screenshot(bytes, cx),
                    None => {
                        self.browser.update(cx, |browser, _| browser.shot_pending.insert(key));
                    }
                }
            }
            WebviewIntent::Annotate(_) => {}
            // D6 (docs/43 §2.7): the nav row's "open in Browse" carries the
            // session's page into Browse, full width.
            WebviewIntent::OpenInBrowse => self.carry_session_tab_to_browse(window, cx),
            WebviewIntent::SendAnnotations {
                annotations,
                screenshot,
                url,
                pins,
                page_size,
            } => {
                // The screenshot still rides as an image the agent can see;
                // the batch (D4) remembers it, the pins and the notes so the
                // sent turn draws as one annotation turn.
                let session = self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone());
                let shot_path = match (&screenshot, &session) {
                    (Some(bytes), Some(session)) => crate::sent::save_picture(session, "png", bytes),
                    _ => None,
                };
                if let Some(bytes) = screenshot {
                    self.attach_browser_shot_named(ANNOTATION_SHOT_NAME, bytes, cx);
                }
                let batch = annotation_batch(&annotations, &url, &pins, page_size, shot_path);
                let block = annotations_draft_block(&annotations, &url);
                match self.active.clone() {
                    Some(view) => {
                        view.update(cx, |view, cx| {
                            view.append_draft_block(block, window, cx);
                            view.runtime().clone().update(cx, |rt, cx| rt.set_annotation_draft(batch, cx));
                        });
                        // The batch now lives in the composer: the panel
                        // empties, as the spec's "the batch becomes the
                        // annotation turn" means (owner 2026-10-21: the
                        // list stayed after Send). Annotate stays on.
                        let key = self.browser_key(cx);
                        let state = self.browser_for(&key, false, window, cx);
                        state.update(cx, |state, cx| state.clear_annotations(cx));
                        cx.notify();
                    }
                    None => {
                        self.overlays.update(cx, |overlays, cx| {
                            overlays.toast("Browser", "No open session to attach the annotations to.", cx);
                        });
                    }
                }
            }
        }
    }


    #[cfg(feature = "probe")]
    /// Rebuild the shown tab's page, consuming queued fake-backend scripts
    /// (the `--steps` capture seam; production never queues). Answers
    /// whether a page was rebuilt.
    pub(super) fn rebuild_shown_browse_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else { return false };
        self.browser.update(cx, |browser, _| browser.states.remove(&id));
        self.terminal_service.unregister_tab_page(id.0);
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        true
    }
}

impl BrowserView {
    /// Start a fetch for every tab favicon not yet asked for this launch
    /// ([`favicons`]); each landing repaints the sidebar through the
    /// favicon generation in its key.
    pub(super) fn fetch_favicons(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<String> = self.tabs.tabs.iter().filter_map(|tab| tab.favicon.clone()).collect();
        for url in wanted {
            if !self.favicons.needs_fetch(&url) {
                continue;
            }
            let fetch_url = url.clone();
            let task = cx.background_executor().spawn(async move { favicons::fetch(&fetch_url) });
            cx.spawn(async move |this, cx| {
                let path = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.favicons.landed(url, path);
                    cx.notify();
                });
            })
            .detach();
        }
    }
    /// Drain popups and "open in new tab" requests for every live tab, on
    /// the page reconcile (a page reports its popup as a change): each becomes a new tab right after its opener, with the
    /// opener's owner and space, wrapping the given backend for popups — a
    /// fresh tab at that URL for open requests. Only the shown tab's
    /// popups may take the centre: a foreground ask from a background
    /// opener lands in the background instead.
    ///
    /// Answers whether any tab was placed. Each placement redraws the view,
    /// so the root's next render pushes the new row into the cached
    /// sidebar's facts (`Harness::compose_sidebar`): a
    /// popup drained only by the frame waited for the next input —
    /// ⌘-click and the menu's "Open Link in New Tab" showed nothing until
    /// the person clicked again (owner, 9 Nov).
    pub(super) fn drain_browse_popups(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.tabs_loaded {
            return false;
        }
        let mut placed_any = false;
        let shown = self.browse_centre_shown(cx);
        let openers: Vec<TabId> = self.states.keys().copied().collect();
        for opener in openers {
            let Some(state) = self.states.get(&opener).cloned() else { continue };
            let popups = state.update(cx, |state, _| state.take_popup_backends());
            for (url, foreground, backend) in popups {
                let foreground = chrome::popup_foreground(foreground, opener, shown);
                let Some(id) = chrome::place_popup_tab(&mut self.tabs, opener, &url, foreground) else { continue };
                let page = cx.new(|cx| WebviewState::new(backend, cx));
                self.adopt_page(id, page, cx);
                self.find_strips.remove(id);
                self.tabs_changed(cx);
                placed_any = true;
            }
            let opens = state.update(cx, |state, _| state.take_open_requests());
            for (url, foreground) in opens {
                let asked = foreground;
                let foreground = chrome::popup_foreground(foreground, opener, shown);
                let before = self.tabs.get(opener).and_then(|tab| self.tabs.active_of(&tab.owner, tab.space.as_deref()));
                let placed = chrome::place_popup_tab(&mut self.tabs, opener, &url, foreground);
                let after = self.tabs.get(opener).and_then(|tab| self.tabs.active_of(&tab.owner, tab.space.as_deref()));
                crate::baaz_log!(
                    "browse: open request {url} from tab {} (asked foreground={asked}, shown={:?}) → tab {:?}, active {:?} → {:?}",
                    opener.0, shown.map(|id| id.0), placed.map(|id| id.0), before.map(|id| id.0), after.map(|id| id.0)
                );
                if placed.is_some() {
                    self.tabs_changed(cx);
                    placed_any = true;
                }
            }
        }
        placed_any
    }


    /// Whether this session key has a live page: its active tab's state
    /// exists. The existing pane tests pin boot and switch on this.
    #[cfg(test)]
    pub(crate) fn session_has_page(&self, session_id: &str) -> bool {
        self.session_page(session_id).is_some()
    }


    /// The session's live page: its active tab's state, when built. The
    /// existing pane tests reach the session's page through this instead
    /// of the tab-keyed registry.
    #[cfg(test)]
    pub(crate) fn session_page(&self, session_id: &str) -> Option<Entity<WebviewState>> {
        let owner = Owner::Session(session_id.to_owned());
        self.tabs.active_of(&owner, None).and_then(|id| self.states.get(&id).cloned())
    }
}
