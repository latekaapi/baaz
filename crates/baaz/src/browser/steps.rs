//! Browse's `--steps` verbs (task 4.5 split of `browser/mod.rs`): the
//! scripted capture verbs for tabs, the Field, find, permissions,
//! downloads, history and bookmarks. Capture-only (audit F20).

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    /// `browse:<url>`: navigate the active (or home) browser, idempotently —
    /// navigating to the shown URL is a no-op that succeeds (the page is
    /// already there). An empty URL is an `Err`. Free: no turn, no wire.
    pub(crate) fn step_browse(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let url = rest.trim();
        if url.is_empty() {
            return Err("no URL given".into());
        }
        let key = self.browser_key(cx);
        let owner = self.browser.read(cx).tab_owner_for_key(&key);
        let state = self.browser_for(&key, false, window, cx);
        if state.read(cx).url().as_ref() != url {
            let title = state.read(cx).title().to_string();
            state.update(cx, |state, _cx| state.navigate(url));
            let space = self.browser.read(cx).owner_space(&owner);
            if let Some(id) = self.browser.read(cx).tabs.active_of(&owner, space.as_deref()) {
                if self.browser.update(cx, |browser, _| browser.tabs.set_url_title(id, url, &title)) {
                    self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                }
            }
        }
        cx.notify();
        Ok(())
    }

    /// `tab-new:<url>`: open a tab on `url` — a person tab in Browse
    /// mode, otherwise a tab for the shown owner (a session tab in the
    /// active session, a person tab with no session open) — and select
    /// it. An empty payload is an `Err`. A blank URL focuses the
    /// address field, like a by-hand open. Free: no turn, no wire.
    pub(crate) fn step_tab_new(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.open_new_tab(rest, window, cx)
    }

    /// `tab-select:<n>`: select the n-th tab of the shown strip (1-based).
    /// An unparseable or out-of-range `n` is an `Err`. Free.
    pub(crate) fn step_tab_select(
        &mut self,
        rest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::steps::StepResult {
        let (key, id) = self.strip_tab_at(rest, cx).ok_or_else(|| format!("no tab `{rest}` in the shown strip"))?;
        self.select_tab(&key, id, window, cx);
        Ok(())
    }

    /// `tab-close:<n>`: close the n-th tab of the shown strip (1-based),
    /// activating its neighbour. An unparseable or out-of-range `n` is an
    /// `Err`. Free.
    pub(crate) fn step_tab_close(
        &mut self,
        rest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> crate::steps::StepResult {
        let (key, id) = self.strip_tab_at(rest, cx).ok_or_else(|| format!("no tab `{rest}` in the shown strip"))?;
        self.close_tab(&key, id, window, cx);
        Ok(())
    }

    /// `tab-pin:<n>`: pin (or unpin, when already pinned) the n-th
    /// person tab of the current project (1-based, on-screen order).
    /// An unparseable or out-of-range `n` is an `Err`. Free.
    pub(crate) fn step_tab_pin(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = self.browse_tab_at(rest, cx).ok_or_else(|| format!("no person tab `{rest}`"))?;
        self.toggle_pin_browse_tab(id, cx);
        Ok(())
    }

    /// `tab-menu:<n>`: open the n-th person tab's right-click menu
    /// (1-based, on-screen order). An unparseable or out-of-range `n` is
    /// an `Err`. Free.
    pub(crate) fn step_tab_menu(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = self.browse_tab_at(rest, cx).ok_or_else(|| format!("no person tab `{rest}`"))?;
        let (pinned, in_folder) = self.browser.read(cx).tabs.get(id).map_or((false, false), |tab| (tab.pinned, tab.folder.is_some()));
        // A right-click lands mid-sidebar at a fixed height: the menu
        // seats at the pointer, so the capture shows it over the page.
        let menu = aui::screens::sidebar::BrowseMenu {
            target: aui::screens::sidebar::BrowseMenuTarget::Tab { id: format!("tab-{}", id.0).into(), pinned, in_folder },
            at: gpui::point(gpui::px(150.0), gpui::px(300.0)),
        };
        self.open_browse_menu(menu, cx);
        Ok(())
    }

    /// `folder-new:<name>`: a new folder in the current space holding the
    /// active person tab, named `<name>` (the new-folder flow, committed).
    /// A blank name, or no active person tab to put in it, is an `Err` and
    /// makes no folder.
    pub(crate) fn step_folder_new(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let name = rest.trim();
        if name.is_empty() {
            return Err("no folder name given".into());
        }
        self.ensure_tabs_loaded(cx);
        let space = self.browser.read(cx).browse_space().unwrap_or_default();
        let tab = self
            .browser
            .read(cx)
            .tabs
            .active_of(&Owner::Person, Some(&space))
            .ok_or("no active person tab to put in the folder")?;
        let id = self.browser.update(cx, |browser, _| browser.tabs.add_folder(&space, name));
        let moved = self.browser.update(cx, |browser, _| browser.tabs.set_folder(tab, Some(&id)));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        if moved { Ok(()) } else { Err("the active tab did not move into the new folder".into()) }
    }

    /// `space-new:<name>`: add a space named `<name>` (it becomes current).
    /// A blank name is the by-hand default, "Space N" — still a new space,
    /// so it succeeds.
    pub(crate) fn step_space_new(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.ensure_tabs_loaded(cx);
        self.browser.update(cx, |browser, _| browser.tabs.add_space(rest.trim()));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        Ok(())
    }

    /// Resolve `rest` to the current project's n-th person tab (1-based,
    /// on-screen order): what `tab-pin:` and `tab-menu:` index into.
    /// `None` for an unparseable or out-of-range `n`.
    pub(super) fn browse_tab_at(&mut self, rest: &str, cx: &mut gpui::App) -> Option<TabId> {
        self.ensure_tabs_loaded(cx);
        let n: usize = rest.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let space = self.browser.read(cx).browse_space();
        self.browser.read(cx).tabs.person_tabs(space.as_deref()).get(n - 1).map(|tab| tab.id)
    }

    /// Resolve `rest` to the shown strip's n-th tab (1-based): the session
    /// key plus the tab id. `None` for an unparseable or out-of-range `n`.
    pub(super) fn strip_tab_at(&mut self, rest: &str, cx: &mut Context<Self>) -> Option<(String, TabId)> {
        self.ensure_tabs_loaded(cx);
        let n: usize = rest.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let owner = self.shown_tab_owner(cx);
        let space = self.browser.read(cx).owner_space(&owner);
        let id = self
            .browser
            .read(cx)
            .tabs
            .ordered_tabs(&owner, space.as_deref())
            .get(n - 1)
            .map(|tab| tab.id)?;
        Some((self.browser_key(cx), id))
    }

    /// `tab-discard-now`: drop every hidden tab's live page at once,
    /// ignoring the idle timer (audible and agent-active pages are still
    /// spared). The live-check hook. Nothing hidden to drop is still
    /// done (every hidden page is gone), so it always succeeds. Free.
    pub(crate) fn step_tab_discard_now(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let space = self.browser.read(cx).owner_space(&owner);
        let shown = self.browser.read(cx).tabs.active_of(&owner, space.as_deref());
        let centre = self.browser.read(cx).browse_centre_shown(cx);
        self.discard_hidden_tabs(true, shown, centre, cx);
        cx.notify();
        Ok(())
    }

    /// `history:<url>|<title>|<days ago>|<page words>`: record a visit to
    /// `url` `days ago` days back (blank = now), with optional page text for
    /// the omnibox's full-text match. An unrecordable URL or an unparseable
    /// day count is an `Err`. Free: no turn, no wire.
    pub(crate) fn step_history(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let mut parts = rest.splitn(4, '|');
        let url = parts.next().unwrap_or("").trim().to_owned();
        let title = parts.next().unwrap_or("").trim().to_owned();
        let days: u64 = match parts.next().map(str::trim) {
            None | Some("") => 0,
            Some(days) => days.parse().map_err(|_| format!("`{days}` is not a day count"))?,
        };
        let words = parts.next().unwrap_or("").trim().to_owned();
        let when = tabs::now_ms().saturating_sub(days * 86_400_000);
        // What `record_visit` refuses, refused here (the write itself runs
        // in the `BrowserData` worker; a failed write is logged there).
        if !history::recordable(&url) || self.browser.read(cx).data.read(cx).is_excluded(&url) {
            return Err(format!("`{url}` could not be recorded as a visit"));
        }
        self.browser.read(cx).data.clone().update(cx, |data, _| {
            data.record_visit(&url, &title, when);
            if !words.is_empty() {
                data.set_text(&url, &title, &words);
            }
        });
        cx.notify();
        Ok(())
    }

    /// `bookmark:<url>|<title>|<folder>`: bookmark `url` on the bar, or in
    /// the folder named `<folder>` (made when missing). An unrecordable
    /// URL, a folder that cannot be found or made, or a refused add is an
    /// `Err`. Free: no turn, no wire.
    pub(crate) fn step_bookmark(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let mut parts = rest.splitn(3, '|');
        let url = parts.next().unwrap_or("").trim().to_owned();
        let title = parts.next().unwrap_or("").trim().to_owned();
        let folder_name = parts.next().unwrap_or("").trim().to_owned();
        if !history::recordable(&url) {
            return Err(format!("`{url}` cannot be bookmarked"));
        }
        self.browser.read(cx).data.clone().update(cx, |data, cx| {
            let folder = if folder_name.is_empty() {
                bookmarks::BAR
            } else {
                match data.folders().into_iter().find(|f| f.name == folder_name) {
                    Some(folder) => folder.id,
                    None => data.add_folder(&folder_name, None, cx),
                }
            };
            data.add_bookmark(&url, &title, folder, tabs::now_ms(), cx);
        });
        Ok(())
    }

    /// `bookmarks`: enter Browse and open the address field scoped to
    /// bookmarks. No page in the centre to anchor the field is an `Err`.
    /// Free.
    pub(crate) fn step_bookmarks(&mut self, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.ensure_tabs_loaded(cx);
        if self.browse_centre_page(window, cx).is_none() {
            return Err("no page is open in Browse".into());
        }
        self.focus_browse_address_with("", window, cx);
        self.browser.update(cx, |browser, cx| browser.set_field_scope(Some(None), cx));
        Ok(())
    }

    /// `omnibox:<text>`: enter Browse, focus the address field and type
    /// `text` — the `browse-omnibox` capture aid; the dropdown stays open
    /// on the typed text. No page in the centre is an `Err`. Free.
    pub(crate) fn step_omnibox(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.ensure_tabs_loaded(cx);
        if self.browse_centre_page(window, cx).is_none() {
            return Err("no page is open in Browse".into());
        }
        self.focus_browse_address_with(rest, window, cx);
        Ok(())
    }

    /// `find:<query>`: open the find strip on the shown tab with `query`
    /// typed (a scripted "3 of 12" result lands through the poll path, so
    /// the `browse-find` capture shows the count). An empty query, no
    /// shown tab, or a find strip that did not open is an `Err`. Free: no
    /// turn, no wire.
    pub(crate) fn step_find(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let query = rest.trim();
        if query.is_empty() {
            return Err("no query given".into());
        }
        self.browser.update(cx, |browser, _| browser.fake_scripts.push(chrome::FakePageScript::Find { query: query.to_owned(), active: 3, total: 12 }));
        if !self.rebuild_shown_browse_page(window, cx) {
            return Err("no tab is shown in Browse".into());
        }
        self.open_browse_find(window, cx);
        let input = self.browser.read(cx).browse_find.clone().ok_or("the find strip did not open")?;
        let query = query.to_owned();
        input.update(cx, |input, cx| input.set_value(query, window, cx));
        self.find_input_changed(cx);
        Ok(())
    }

    /// `permission:<kinds>`: script a permission ask (`camera,microphone`)
    /// on the shown tab, so the `browse-permission` capture shows the
    /// strip. Unknown kinds, or no shown tab, is an `Err`. Free.
    pub(crate) fn step_permission(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let kinds = chrome::parse_permission_kinds(rest).ok_or_else(|| format!("`{rest}` names an unknown permission"))?;
        self.browser.update(cx, |browser, _| {
            browser.fake_scripts.push(chrome::FakePageScript::Permission {
                id: 7,
                origin: "https://example.com".to_owned(),
                kinds,
            })
        });
        if !self.rebuild_shown_browse_page(window, cx) {
            return Err("no tab is shown in Browse".into());
        }
        Ok(())
    }

    /// `download:<name>`: script an active download (`done:<name>` lands
    /// it) on the shown tab and open the popover, so the
    /// `browse-downloads` capture shows the list. An empty name, or no
    /// shown tab, is an `Err`. Free.
    pub(crate) fn step_download(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let (done, name) = match rest.trim().strip_prefix("done:") {
            Some(done) => (true, done.trim()),
            None => (false, rest.trim()),
        };
        if name.is_empty() {
            return Err("no file name given".into());
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let script = chrome::FakePageScript::Download(aui_webview::DownloadState {
            id: 3,
            url: format!("https://cdn.example/{name}"),
            file_name: name.to_owned(),
            path: format!("{home}/Downloads/{name}"),
            received: if done { 10_485_760 } else { 4_400_000 },
            total: Some(10_485_760),
            state: if done { aui_webview::DownloadStatus::Complete } else { aui_webview::DownloadStatus::Downloading },
        });
        self.browser.update(cx, |browser, _| browser.fake_scripts.push(script));
        if !self.rebuild_shown_browse_page(window, cx) {
            return Err("no tab is shown in Browse".into());
        }
        self.overlays.update(cx, |o, cx| o.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::Downloads), cx));
        self.browser.update(cx, |browser, _| browser.downloads_rendered = false);
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::Extensions, cx); });
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::More, cx); });
        Ok(())
    }

    /// `crashed:<reason>`: script a renderer crash on the shown tab, so
    /// the `browse-crashed` capture shows the Baaz error page. A blank
    /// reason uses a default one; no shown tab is an `Err`. Free.
    pub(crate) fn step_crashed(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let reason = rest.trim();
        let reason = if reason.is_empty() { "the page crashed (-1)" } else { reason };
        self.browser.update(cx, |browser, _| browser.fake_scripts.push(chrome::FakePageScript::Crashed(reason.to_owned())));
        if !self.rebuild_shown_browse_page(window, cx) {
            return Err("no tab is shown in Browse".into());
        }
        Ok(())
    }

    /// `more-menu`: open the ⋯ page-actions menu on the shown tab, so the
    /// `browse-more-menu` capture shows it. No shown tab is an `Err`.
    /// Free.
    pub(crate) fn step_more_menu(&mut self, _rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        if self.browse_centre_page(window, cx).is_none() {
            return Err("no page is open in Browse".into());
        }
        self.overlays.update(cx, |o, cx| o.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::More), cx));
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::Downloads, cx); });
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::Extensions, cx); });
        Ok(())
    }

    /// `extensions`: open the extensions popover on the shown tab. No
    /// shown tab is an `Err`. Free.
    pub(crate) fn step_extensions(&mut self, _rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        if self.browse_centre_page(window, cx).is_none() {
            return Err("no page is open in Browse".into());
        }
        self.overlays.update(cx, |o, cx| o.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::Extensions), cx));
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::Downloads, cx); });
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::More, cx); });
        Ok(())
    }

    /// `fixture-session:<name>`: park a local session row plus one session
    /// tab carrying `https://example.com`, and carry it into Browse — the
    /// offline `browse-from-session` capture aid. A blank name uses
    /// "Fix ledger chart". Always succeeds. Free: no turn, no wire.
    pub(crate) fn step_fixture_session(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let name = rest.trim();
        let name = if name.is_empty() { "Fix ledger chart" } else { name };
        let id = format!("fixture-{}", name.to_lowercase().replace(' ', "-"));
        self.ensure_tabs_loaded(cx);
        if !self.session_list.read(cx).sessions.iter().any(|entry| entry.id == id) {
            let now = chrono::Local::now();
            let space = self.browser.read(cx).browse_space();
            let workspace =
                self.current_project(cx).map(|project| project.root.to_string_lossy().into_owned());
            self.session_list.update(cx, |list, _| list.sessions.push(crate::sidebar::local_started_row(&id, name.to_owned(), space.clone(), workspace, now)));
            self.invalidate_list(cx);
        }
        let space = self.browser.read(cx).browse_space();
        let tab_id = self.browser.update(cx, |browser, _| browser.tabs.open(space, Owner::Session(id.clone()), "https://example.com", None));
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        // The agent's tools can reach it, so a live check needs no provider.
        let root = self.current_project(cx).map(|project| project.root.clone()).unwrap_or_else(std::env::temp_dir);
        self.terminal_service.register_session(&id, root);
        self.ensure_tab_page(&id, tab_id, window, cx);
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.browser.update(cx, |browser, cx| browser.set_session_tab(Some(tab_id), cx));
        Ok(())
    }

    /// `page-focus`: give the active browser page the keyboard, focusing the
    /// pane's own focus handle — the same hand-over an explicit person open
    /// performs. In Browse with no centre page it is an `Err`. Whether the
    /// native page then takes the keyboard is the backend's, and settles
    /// later: it is logged, not judged, so the hand-over itself succeeds.
    /// Free: no turn, no wire.
    pub(crate) fn step_page_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        // In Browse the active page is the centre's, not the pane's.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.ensure_browse_tab(window, cx);
            let state = self.browse_centre_page(window, cx).ok_or("no page is open in Browse")?;
            let focus = state.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
            return Ok(());
        }
        let key = self.browser_key(cx);
        let state = self.browser_for(&key, false, window, cx);
        let focus = state.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        if state.read(cx).holds_keyboard() {
            crate::baaz_log!("browser: page focused");
        } else {
            crate::baaz_log!("browser: page-focus: page did not take the keyboard");
        }
        Ok(())
    }
}
