//! Browse's tab actions on the window (task 4.5 split of `browser/mod.rs`): the
//! tab keys and owners, the Browse sidebar's tab rows (new, select, close,
//! pin, reopen, the ⌘1–9 / ⌘W / ⌘⇧T shortcuts) and the session tabs.

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {
    /// Open a tab on `url` — a person tab in Browse mode, otherwise a tab
    /// for the shown owner (a session tab in the active session, a person
    /// tab with no session open) — and select it: links from other apps,
    /// the Changes pane's PR link, Settings' Chromium page, and the
    /// `tab-new:` step. An empty `url` is an `Err`. A blank URL focuses the
    /// address field, like a by-hand open.
    pub(crate) fn open_new_tab(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let url = rest.trim();
        if url.is_empty() {
            return Err("no URL given".into());
        }
        // A real page opening ends any open address edit (the blank tab
        // entering Browse opens one), exactly as selecting a tab does.
        if url != BLANK {
            self.leave_browse_address(window, cx);
        }
        self.ensure_tabs_loaded(cx);
        // B2a-H2b: in Browse a new tab is always the person's, even with
        // a session open behind the mode — the session's tabs belong to
        // Sessions.
        let owner = if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            Owner::Person
        } else {
            self.shown_tab_owner(cx)
        };
        let space = self.browser.read(cx).browse_space();
        let after = self.browser.read(cx).tabs.active_of(&owner, space.as_deref());
        // D6 (owner 2026-10-17): a session has one page — a new page for a
        // session navigates the one it has instead of opening a second.
        if let (Owner::Session(_), Some(id)) = (&owner, after) {
            if url != BLANK {
                if let Some(state) = self.browser.read(cx).states.get(&id).cloned() {
                    state.update(cx, |state, _| state.navigate(url));
                    if self.browser.update(cx, |browser, _| browser.tabs.set_url_title(id, url, url)) {
                        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                    }
                    cx.notify();
                    return Ok(());
                }
            }
        }
        let id = self.browser.update(cx, |browser, _| browser.tabs.open(space, owner.clone(), url, after));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        // In Browse the person's new tab is what the centre shows next, over
        // any session tab it held.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        }
        let key = match &owner {
            Owner::Person => HOME_KEY.to_owned(),
            Owner::Session(_) => self.browser_key(cx),
        };
        let state = self.ensure_tab_page(&key, id, window, cx);
        if url == BLANK {
            state.update(cx, |state, cx| state.begin_editing(window, cx));
        }
        Ok(())
    }

    /// The registry key whose webview the pane shows: the active session's
    /// id, or the home key with no session open.
    pub(crate) fn browser_key(&self, cx: &gpui::App) -> String {
        self.active
            .as_ref()
            .map(|view| view.read(cx).rt(cx).session_id.clone())
            .unwrap_or_else(|| HOME_KEY.to_owned())
    }

    /// Whose tabs the pane shows: the active session's, or the person's
    /// with no session open.
    pub(crate) fn shown_tab_owner(&self, cx: &gpui::App) -> Owner {
        self.browser.read(cx).tab_owner_for_key(&self.browser_key(cx))
    }

    /// Load the tab model on first use and migrate older restores: every
    /// session with a stored `browser_url` and no tabs yet gets one
    /// session tab with that URL. Persists when either step changed the
    /// model.
    pub(crate) fn ensure_tabs_loaded(&mut self, cx: &mut gpui::App) {
        if self.browser.read(cx).tabs_loaded {
            return;
        }
        self.browser.update(cx, |browser, _| browser.tabs_loaded = true);
        self.browser.update(cx, |browser, _| browser.tabs = tabs::load());
        let list = self.session_list.clone();
        let mut changed = self.browser.update(cx, |browser, cx| browser.tabs.migrate_from_overrides(&list.read(cx).overrides));
        // Spaces are the browser's own (docs/27 v2): an older file's
        // per-project person tabs become Personal's folders, once.
        self.browser.update(cx, |browser, cx| {
            let projects = &list.read(cx).projects;
            changed |= browser.tabs.ensure_spaces(|id| projects.projects.iter().find(|p| p.id == id).map(|p| p.name.clone()))
        });
        if changed {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
    }

    /// The owner's active tab, opening its first when it has none: a
    /// session restores its last URL, everyone starts blank. The strip
    /// shows only when the owner has at least one tab.
    pub(super) fn ensure_owner_tab(&mut self, owner: &Owner, cx: &mut gpui::App) -> TabId {
        let space = self.browser.read(cx).owner_space(owner);
        if let Some(id) = self.browser.read(cx).tabs.active_of(owner, space.as_deref()) {
            return id;
        }
        let stored = match owner {
            Owner::Session(session) => self
                .session_list.read(cx).overrides
                .get(session)
                .and_then(|meta| meta.right.clone())
                .and_then(|right| right.browser_url),
            Owner::Person => None,
        };
        let space = self.browser.read(cx).browse_space();
        let id = self.browser.update(cx, |browser, _| browser.tabs.open(space, owner.clone(), initial_url(stored.as_deref()), None));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        id
    }

    /// Point the agent `browser_*` tools at `state`: the session key's
    /// active tab's page. Called on every resolve and every tab switch, so
    /// registration always follows the active tab.
    pub(super) fn register_tab_page(&mut self, key: &str, id: TabId, state: &Entity<WebviewState>, cx: &mut gpui::App) {
        self.terminal_service.register_browser(key, state.clone());
        self.terminal_service.register_tab_page(key, id.0, state.clone());
        self.browser.update(cx, |browser, _| browser.agent_tabs.insert(key.to_owned(), id));
    }

    /// Entering Browse ensures the project has a person tab to show
    /// (B2a-H2): with none, one blank tab opens with the URL field
    /// focused. A project that already has tabs is untouched — and in
    /// particular never steals focus.
    pub(crate) fn ensure_browse_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let space = self.browser.read(cx).browse_space();
        if self.browser.read(cx).tabs.active_of(&Owner::Person, space.as_deref()).is_none() {
            self.new_browse_sidebar_tab(window, cx);
        }
    }

    /// Open a blank person tab in the current project and select it, with
    /// the address focused: the Browse sidebar's "New tab" row and the
    /// entering-Browse ensure share this one path. Clears any held session
    /// tab: ⌘T lands on the person's new tab.
    pub(crate) fn new_browse_sidebar_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let space = self.browser.read(cx).browse_space();
        let after = self.browser.read(cx).tabs.active_of(&Owner::Person, space.as_deref());
        // Next to the current tab, and in its folder when it sits in one
        // (owner 2026-10-11): a new tab opened from a folder used to land
        // at the end of Today instead.
        let folder = after.and_then(|after| self.browser.read(cx).tabs.get(after)).filter(|tab| !tab.pinned).and_then(|tab| tab.folder.clone());
        let id = self.browser.update(cx, |browser, _| browser.tabs.open(space, Owner::Person, BLANK, after));
        if let Some(folder) = folder {
            self.browser.update(cx, |browser, _| browser.tabs.set_folder(id, Some(&folder)));
        }
        self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        // ⌘T lands on the new-tab page with the field focused (B2a-H4):
        // the Harness-owned address input, empty for a blank tab.
        self.focus_browse_address_with("", window, cx);
    }

    /// Select the Browse sidebar's tab `id` (B2a-H2): the model
    /// activates and the page builds when discarded. A closed — or a
    /// session-owned — `id` is a no-op, so a × click racing the row's
    /// own select never resurrects the tab. The selection moves first and
    /// the address drops after, so focus lands on the newly shown page
    /// (or the root) — never the old page. Selecting a person tab clears a
    /// session tab the centre held.
    pub(crate) fn select_browse_sidebar_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        if self.browser.read(cx).tabs.get(id).is_none_or(|tab| tab.owner != Owner::Person) {
            return;
        }
        // The tab being left drops its find highlight (its strip keeps the
        // query), and the shared input points at the arriving tab — both
        // on the event, never during render.
        if let Some(leaving) = self.browser.read(cx).browse_centre_shown(cx) {
            if leaving != id {
                self.browser.update(cx, |browser, cx| browser.stop_find_on_tab(leaving, cx));
            }
        }
        if self.browser.update(cx, |browser, _| browser.tabs.activate(id)) {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        self.browser.update(cx, |browser, cx| browser.clear_drop(cx));
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        self.sync_find_input_to_tab(id, window, cx);
        self.leave_browse_address(window, cx);
    }

    /// Whether the Browse tab shortcuts may fire: only in Browse mode.
    /// The `BaazBrowse` key context scopes them; this is the belt.
    pub(super) fn browse_command_ready(&mut self, cx: &mut Context<Self>) -> Option<Option<String>> {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return None;
        }
        self.ensure_tabs_loaded(cx);
        Some(self.browser.read(cx).browse_space())
    }

    /// ⌘W in Browse: while a session tab shows, stop showing it —
    /// back to the person's active tab, closing nothing. Otherwise close
    /// the current project's active person tab, the model picking the
    /// display-order neighbour. A no-op outside Browse (where ⌘W keeps
    /// closing the window) or with no tab.
    pub(crate) fn close_browse_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        if self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx)) {
            return;
        }
        let owner = Owner::Person;
        let Some(id) = self.browser.read(cx).tabs.active_of(&owner, space.as_deref()) else {
            return;
        };
        // ⌘W on the space's last tab closes the window, as a browser does
        // (owner 2026-10-13): the tab goes (a real page joins the closed
        // list for ⌘⇧T; a blank one does not), a fresh blank tab stands
        // ready for the window's return, and the window hides the way
        // ⌘W in Sessions does.
        let last = self.browser.read(cx).tabs.person_tabs(space.as_deref()).len() <= 1;
        self.close_tab(HOME_KEY, id, window, cx);
        if last {
            window.dispatch_action(Box::new(crate::app::CloseWindow), cx);
        }
    }

    /// ⌘⇧T in Browse: reopen the most recently closed tab, selecting it
    /// for its owner and showing it when it is this project's person
    /// tab. A no-op outside Browse or with nothing closed.
    pub(crate) fn reopen_browse_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        let Some(id) = self.browser.update(cx, |browser, _| browser.tabs.reopen_last()) else {
            return;
        };
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        let ours = self.browser.read(cx).tabs.get(id).is_some_and(|tab| {
            tab.owner == Owner::Person && tab.space.as_deref() == space.as_deref()
        });
        if ours {
            self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
            self.ensure_tab_page(HOME_KEY, id, window, cx);
        }
    }

    /// ⌘1–⌘8 (a 1-based display slot) and ⌘9 (the last tab) in Browse:
    /// select that person tab. Unknown slots are a no-op, as is firing
    /// outside Browse.
    pub(crate) fn select_browse_nth(
        &mut self,
        n: usize,
        last: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        let owner = Owner::Person;
        let Some(id) = self.browser.read(cx).tabs.nth_tab(&owner, space.as_deref(), n, last) else {
            return;
        };
        self.select_browse_sidebar_tab(id, window, cx);
    }

    /// ⌥⌘↑/⌥⌘↓ in Browse: move the active person tab one display slot.
    /// A no-op while a session tab shows (the moves act on the
    /// person's hidden tab otherwise), at the edge, across the pinned
    /// boundary, or outside Browse.
    pub(crate) fn move_browse_shortcut(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        if self.browser.read(cx).browse_session_tab.is_some() {
            return;
        }
        if self.browser.update(cx, |browser, _| browser.tabs.move_active_by(&Owner::Person, space.as_deref(), delta)) {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        let _ = window;
    }

    /// Close the person tab `id` from the Browse sidebar's ×: the
    /// model's neighbour rule picks what shows next, and closing the
    /// project's last tab opens a fresh blank one (see
    /// [`Self::close_tab`]). A closed or session-owned `id` is a no-op.
    pub(crate) fn close_person_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if self.browser.read(cx).tabs.get(id).is_none_or(|tab| tab.owner != Owner::Person) {
            return;
        }
        self.browser.update(cx, |browser, cx| browser.clear_drop(cx));
        self.close_tab(HOME_KEY, id, window, cx);
    }

    #[cfg(feature = "probe")]
    /// Pin or unpin the person tab `id`: the tab menu's Pin/Unpin row
    /// and the `tab-pin:<n>` step. A closed or session-owned `id` is a
    /// no-op. Answers the new pinned state.
    pub(crate) fn toggle_pin_browse_tab(&mut self, id: TabId, cx: &mut Context<Self>) -> Option<bool> {
        self.ensure_tabs_loaded(cx);
        let pinned = !self.browser.read(cx).tabs.get(id).filter(|tab| tab.owner == Owner::Person)?.pinned;
        self.browser.update(cx, |browser, _| browser.tabs.set_pinned(id, pinned)).then_some(pinned)?;
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        Some(pinned)
    }

    /// Reload the person tab `id` at its own URL and show it: the tab
    /// menu's Reload row, and what a crashed row's click does (the `!`
    /// tile's promise is reload *and* select). Activating clears a held
    /// session tab, like every person-tab select. When the tab has
    /// no live page the ensure rebuilds it anyway. A closed or
    /// session-owned `id` is a no-op.
    pub(crate) fn reload_browse_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(tab) = self.browser.read(cx).tabs.get(id).cloned() else {
            return;
        };
        if tab.owner != Owner::Person {
            return;
        }
        if self.browser.update(cx, |browser, _| browser.tabs.activate(id)) {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        let state = self.ensure_tab_page(HOME_KEY, id, window, cx);
        let url = tab.url.clone();
        if !url.is_empty() {
            state.update(cx, |state, _cx| state.navigate(&url));
        }
        self.browser.update(cx, |browser, cx| browser.clear_drop(cx));
        cx.notify();
    }

    /// Show a session-owned tab in the Browse centre (a task-folder row):
    /// the model activates it for its session and the centre builds its
    /// page — ownership never moves. A closed or person-owned `id` is a
    /// no-op.
    pub(crate) fn select_session_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if self.browser.read(cx).tabs.get(id).is_none_or(|tab| !matches!(tab.owner, Owner::Session(_))) {
            return;
        }
        if let Some(leaving) = self.browser.read(cx).browse_centre_shown(cx) {
            if leaving != id {
                self.browser.update(cx, |browser, cx| browser.stop_find_on_tab(leaving, cx));
            }
        }
        if self.browser.update(cx, |browser, _| browser.tabs.activate(id)) {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        self.browser.update(cx, |browser, cx| browser.set_session_tab(Some(id), cx));
        self.ensure_tab_page(HOME_KEY, id, window, cx);
    }

    /// A Browse sidebar row's select or close, routed by the tab's owner:
    /// person tabs through the person paths, task-folder tabs through
    /// [`Self::select_session_tab`] / [`Self::close_tab`] under their
    /// session's key. An unknown `id` is a no-op.
    pub(crate) fn browse_sidebar_tab(&mut self, id: TabId, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(owner) = self.browser.read(cx).tabs.get(id).map(|tab| tab.owner.clone()) else {
            return;
        };
        match (owner, close) {
            (Owner::Person, false) => self.select_browse_sidebar_tab(id, window, cx),
            (Owner::Person, true) => self.close_person_tab(id, window, cx),
            (Owner::Session(_), false) => self.select_session_tab(id, window, cx),
            (Owner::Session(session), true) => {
                if self.browser.read(cx).browse_session_tab == Some(id) {
                    self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
                }
                self.close_tab(&session, id, window, cx);
            }
        }
    }

    #[cfg(feature = "probe")]
    /// Select `id` for its owner: the model activates, the page builds when
    /// discarded, and the agent tools re-register onto it. A closed `id`
    /// is a no-op, so a × click racing the row's own select never
    /// resurrects the tab.
    pub(crate) fn select_tab(
        &mut self,
        key: &str,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        if self.browser.read(cx).tabs.get(id).is_none() {
            return;
        }
        if self.browser.update(cx, |browser, _| browser.tabs.activate(id)) {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        self.ensure_tab_page(key, id, window, cx);
    }

    /// Close `id`: the closed page is dropped; only when the closed tab
    /// was its owner's active tab does anything else move — the model
    /// picks the neighbour to activate and the agent tools follow it (or
    /// lose the session's page when none is left). Closing a background
    /// tab rebuilds and re-registers nothing. A session's last tab also
    /// clears its stored `browser_url`, so the tab never resurrects from
    /// the old restore — and the migration runs only once per session, so
    /// it stays closed.
    pub(crate) fn close_tab(
        &mut self,
        key: &str,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        let Some(tab) = self.browser.read(cx).tabs.get(id).cloned() else {
            return;
        };
        // The tab's OWN owner+space: [`Tabs::close`] keys the active
        // pointer on them, and the current project's space is wrong when
        // the close arrives from another space (a person tab closed while
        // a different project is current).
        let was_active = self
            .browser
            .read(cx)
            .tabs
            .active_of(&tab.owner, tab.space.as_deref())
            == Some(id);
        let next = self.browser.update(cx, |browser, _| browser.tabs.close(id));
        self.browser.update(cx, |browser, _| browser.states.remove(&id));
        // The agent tools' registry holds every tab's page (R2's `tab`
        // argument, R3's grants). Left there, a tab the person closed (⌘W,
        // ✕) kept its page alive: the page parks its native view only when
        // dropped, so the closed page stayed on screen over the next tab and
        // read as a frozen window (owner, 9 Nov). Whoever closes, it goes.
        self.terminal_service.unregister_tab_page(id.0);
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        // B2a-H2b: closing the current project's last person tab while
        // in Browse opens a fresh blank one — entering Browse and this
        // close are the two events that create tabs, never render. Other
        // spaces and Sessions mode keep the closed state: emptiness
        // there is honest, and the centre is not showing.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse
            && tab.owner == Owner::Person
            && tab.space == self.browser.read(cx).browse_space()
            && self.browser.read(cx).tabs.person_tabs(tab.space.as_deref()).is_empty()
        {
            self.new_browse_sidebar_tab(window, cx);
            return;
        }
        if !was_active {
            return;
        }
        match next {
            Some(next) => {
                self.ensure_tab_page(key, next, window, cx);
            }
            None => {
                self.browser.update(cx, |browser, _| browser.agent_tabs.remove(key));
                self.terminal_service.unregister_browser(key);
                if let Owner::Session(session) = tab.owner {
                    if session != key {
                        self.browser.update(cx, |browser, _| browser.agent_tabs.remove(&session));
                        self.terminal_service.unregister_browser(&session);
                    }
                    self.clear_session_browser_url(&session, cx);
                }
            }
        }
    }

    /// Forget a session's stored browser URL (its last tab just closed),
    /// through the cheap debounced write — never the session-list settle.
    /// A no-op when the stored URL is already gone.
    pub(super) fn clear_session_browser_url(&mut self, session: &str, cx: &mut Context<Self>) {
        let mut right = match self
            .session_list.read(cx).overrides
            .get(session)
            .and_then(|meta| meta.right.clone())
        {
            Some(right) => right,
            None => return,
        };
        if right.browser_url.is_none() {
            return;
        }
        right.browser_url = None;
        self.session_list.update(cx, |list, _| {
            if let Some(meta) = list.overrides.get_mut(session) {
                meta.right = Some(right);
            }
        });
        self.schedule_overrides_write(cx);
    }



    /// The pane's "Open in Browse": the session's active tab shows
    /// full-width in Browse mode. The tab stays session-owned; the same
    /// `WebviewState` moves — no reload, the state stays keyed by tab id and
    /// the centre renders it through `browse_centre_page`.
    pub(crate) fn carry_session_tab_to_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        if !matches!(owner, Owner::Session(_)) {
            return;
        }
        let space = self.browser.read(cx).owner_space(&owner);
        let Some(id) = self.browser.read(cx).tabs.active_of(&owner, space.as_deref()) else {
            return;
        };
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.browser.update(cx, |browser, cx| browser.set_session_tab(Some(id), cx));
    }


    /// A session's name for the downloads credit: the
    /// collapsed head label the Sessions header wears.
    pub(crate) fn session_display_name(&self, session: &str, cx: &gpui::App) -> String {
        // A session with no title yet (a draft) reads "New session" — never
        // its raw id.
        self.collapsed_head_label(session, cx)
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| String::from("New session"))
    }

    /// Flip a space's own sign-ins (confirmed): every page in it is dropped
    /// and rebuilds in the other profile when shown (a page cannot move
    /// between profiles). The own profile's data stays on disk.
    pub(crate) fn toggle_space_signins(&mut self, space: &str, cx: &mut Context<Self>) {
        let Some((own, name)) = self.browser.update(cx, |browser, _| {
            let found = browser.tabs.spaces.iter_mut().find(|s| s.id == space)?;
            found.own_signins = !found.own_signins;
            Some((found.own_signins, found.name.clone()))
        }) else { return };
        let in_space: Vec<TabId> = self.browser.read(cx).tabs.tabs.iter().filter(|t| t.space.as_deref() == Some(space)).map(|t| t.id).collect();
        self.browser.update(cx, |browser, cx| browser.close_peek(cx));
        if self.browser.read(cx).split.is_some_and(|p| in_space.contains(&p.left) || in_space.contains(&p.right)) {
            self.browser.update(cx, |browser, _| browser.split = None);
        }
        for id in &in_space {
            if self.browser.update(cx, |browser, _| browser.states.remove(id).is_some()) {
                self.terminal_service.unregister_tab_page(id.0);
            }
        }
        self.browser.update(cx, |browser, _| browser.agent_tabs.retain(|_, id| !in_space.contains(id)));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        crate::baaz_log!("browse: space {space} sign-ins → {}", if own { "its own" } else { "shared" });
        self.overlays.update(cx, |o, cx| {
            o.toast(
                if own { format!("\u{201C}{name}\u{201D} has its own sign-ins") } else { format!("\u{201C}{name}\u{201D} shares sign-ins again") },
                if own { "Its pages reload signed out; sign in here." } else { "" },
             cx)
        });
    }

    /// Reopen the closed tab `id` from a new-tab pick-up row and select it:
    /// selecting clears any carried session tab, so the reopened person tab
    /// shows. A no-op for an unknown id.
    pub(crate) fn reopen_browse_closed_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        if self.browser.update(cx, |browser, _| browser.tabs.reopen(id).is_some()) {
            self.select_browse_sidebar_tab(id, window, cx);
        }
    }
}

impl BrowserView {

    /// The space an owner's active tab lives in: the current space for the
    /// person, nothing for a session (its tabs show in every space).
    pub(super) fn owner_space(&self, owner: &Owner) -> Option<String> {
        match owner {
            Owner::Person => self.browse_space(),
            Owner::Session(_) => None,
        }
    }


    /// Which owner `key` names: a session id names that session's tabs,
    /// [`HOME_KEY`] (no session open) the person's tabs.
    pub(crate) fn tab_owner_for_key(&self, key: &str) -> Owner {
        if key == HOME_KEY {
            Owner::Person
        } else {
            Owner::Session(key.to_owned())
        }
    }

    /// The browser space Browse shows: person tabs are keyed by it, never by
    /// the project (docs/27 §4.2). Before the tab file loads (or in a test
    /// that skips the migration) it reads as Personal.
    pub(crate) fn browse_space(&self) -> Option<String> {
        let current = &self.tabs.current_space;
        Some(if current.is_empty() { tabs::PERSONAL.to_owned() } else { current.clone() })
    }
    /// Queue `tabs.json` behind a tab change: the tabs store's delayed
    /// save (750 ms), so a spell of changes — a title that keeps changing,
    /// a burst of opens — coalesces into one write off the UI thread, the
    /// newest snapshot wins, and a constant stream still lands within the
    /// delay of its first change ([`crate::store::JsonStore`]) — and
    /// redraw: every tab change is drawn (the sidebar's rows, the strip,
    /// the centre), so this is the one notify a tab change makes. The root
    /// reads this view in every frame, so it re-renders too.
    pub(crate) fn tabs_changed(&mut self, cx: &mut Context<Self>) {
        tabs::save_soon(&self.tabs);
        cx.notify();
    }

    /// Land the tabs durably, synchronously: the quit and window-close
    /// durability point, next to the session stores' flush. Writes the
    /// CURRENT model, superseding anything queued. A no-op when the tabs
    /// never loaded, so a quit that never touched the browser writes
    /// nothing; otherwise the in-memory-only `last_shown` stamps land here
    /// at the latest.
    pub(crate) fn flush_tabs_save(&mut self) {
        if !self.tabs_loaded {
            return;
        }
        tabs::write(&self.tabs);
    }

    /// Both quit paths: the tabs land synchronously ([`Self::flush_tabs_save`])
    /// and `browser.db`'s queued writes drain, bounded by
    /// [`BrowserData::QUIT_DRAIN`](super::data::BrowserData::QUIT_DRAIN).
    pub(crate) fn flush_at_quit(&mut self, cx: &gpui::App) {
        self.flush_tabs_save();
        self.data.read(cx).drain_at_quit(super::data::BrowserData::QUIT_DRAIN);
    }

    /// The Chromium profile folder a tab's page lives in: its space's own
    /// (`Spaces/<id>`) when that space keeps its own sign-ins, else the
    /// shared profile (`None`).
    pub(crate) fn tab_profile(&self, id: TabId) -> Option<String> {
        let space = self.tabs.get(id)?.space.clone()?;
        let own = self.tabs.spaces.iter().any(|s| s.id == space && s.own_signins);
        own.then(|| format!("Spaces/{}", space.replace(['/', '.'], "_")))
    }
}
