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

    /// `tab-menu:<n|shown>`: open the n-th person tab's right-click menu
    /// (1-based, on-screen order), or `shown`, the tab Browse shows (a
    /// session's task-folder tab too, with Keep this tab). Its offers fill
    /// in as they draw (`browse_menu_now`). An unparseable or out-of-range
    /// `n`, or no shown tab, is an `Err`. Free.
    pub(crate) fn step_tab_menu(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = if rest.trim() == "shown" {
            self.browser.read(cx).browse_centre_shown(cx).ok_or("no tab is shown in Browse")?
        } else {
            self.browse_tab_at(rest, cx).ok_or_else(|| format!("no person tab `{rest}`"))?
        };
        let (pinned, in_folder) = self.browser.read(cx).tabs.get(id).map_or((false, false), |tab| (tab.pinned, tab.folder.is_some()));
        // A right-click lands mid-sidebar at a fixed height: the menu seats
        // at the pointer. It draws in the overlay window over the page, so
        // a shot of the main window shows the row lit but not the menu.
        let menu = aui::screens::sidebar::BrowseMenu {
            target: aui::screens::sidebar::BrowseMenuTarget::Tab { id: format!("tab-{}", id.0).into(), pinned, in_folder, offers: Default::default() },
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

    /// `site-set:<origin>|<key>=<value>,…`: keep settings for `origin` in
    /// the (capture-only) store — permission answers (`camera=allow`,
    /// `microphone=block`) and `zoom=<percent>` — as the strip's "Always"
    /// answers and the Zoom row keep them. An origin that is not a web
    /// origin, an unknown key or a bad value is an `Err`. Free.
    pub(crate) fn step_site_set(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (origin, pairs) = rest.split_once('|').ok_or_else(|| format!("`{rest}` is not <origin>|<key>=<value>,…"))?;
        let origin = origin.trim();
        if baaz_browser_core::sites::origin_of(origin).as_deref() != Some(origin) {
            return Err(format!("`{origin}` is not a web origin"));
        }
        let mut set = Vec::new();
        for pair in pairs.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').ok_or_else(|| format!("`{pair}` is not <key>=<value>"))?;
            let ok = match key {
                baaz_browser_core::sites::ZOOM_KEY => value.parse::<u16>().is_ok_and(|zoom| (25..=500).contains(&zoom)),
                _ if crate::settings::SITE_PERMISSION_ROWS.iter().any(|(k, _)| *k == key) => baaz_browser_core::sites::Answer::parse(value).is_some(),
                _ => false,
            };
            if !ok {
                return Err(format!("`{pair}` is not a site setting"));
            }
            set.push((key.to_owned(), value.to_owned()));
        }
        if set.is_empty() {
            return Err("no settings given".into());
        }
        self.browser.read(cx).data.clone().update(cx, |data, cx| {
            for (key, value) in &set {
                data.set_site(origin, key, Some(value), cx);
            }
        });
        Ok(())
    }

    /// `zoom:<in|out|reset>`: step the shown Browse page's zoom as ⌘= ⌘-
    /// ⌘0 do, kept for its site. No page, an engine that cannot zoom, an
    /// unknown direction, or a level that did not move is an `Err`. Free.
    pub(crate) fn step_zoom(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let direction = match rest.trim() {
            "in" => 1,
            "out" => -1,
            "reset" => 0,
            other => return Err(format!("`{other}` is not in, out or reset")),
        };
        let state = self.browse_centre_state(cx).ok_or("no page is shown in Browse")?;
        if !state.read(cx).supports(aui_webview::BackendFeature::Zoom) {
            return Err("the shown page's engine cannot zoom".into());
        }
        let url = state.read(cx).url().to_string();
        let kept = |this: &Self, cx: &gpui::App| {
            baaz_browser_core::sites::origin_of(&url).and_then(|origin| this.browser.read(cx).data.read(cx).sites().zoom(&origin)).unwrap_or(100)
        };
        let before = kept(self, cx);
        self.zoom_shown_page(direction, cx);
        if kept(self, cx) == before && direction != 0 {
            return Err(format!("the zoom stayed at {before}%"));
        }
        Ok(())
    }

    /// `agent-drive:<x>,<y>|<label>`: the shown tab's session runs a page
    /// tool on it that never ends, having last acted at `x`,`y` as `label` —
    /// the agent cursor and the driving bar, on the service's own activity
    /// path. No shown page, a tab that is no session's, or a bad point is an
    /// `Err`. Free: the tool sends nothing anywhere.
    pub(crate) fn step_agent_drive(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (point, label) = rest.split_once('|').unwrap_or((rest, "clicking"));
        let (x, y) = point.split_once(',').ok_or_else(|| format!("`{point}` is not <x>,<y>"))?;
        let (x, y): (f64, f64) = (
            x.trim().parse().map_err(|_| format!("`{x}` is not a number"))?,
            y.trim().parse().map_err(|_| format!("`{y}` is not a number"))?,
        );
        let id = self.browser.read(cx).browse_centre_shown(cx).ok_or("no tab is shown in Browse")?;
        let Some(Owner::Session(session)) = self.browser.read(cx).tabs.get(id).map(|tab| tab.owner.clone()) else {
            return Err("the shown tab is no session's (fixture-session: first)".into());
        };
        let state = self.browse_centre_state(cx).ok_or("the shown tab has no page")?;
        self.terminal_service.script_driving(&session, &state, label.trim(), (x, y));
        if self.agent_cursor_spot(cx).is_none() {
            return Err("the agent cursor did not show".into());
        }
        Ok(())
    }

    /// `library:<bookmarks|history>`: open that page over the Browse
    /// centre, idempotently. Unknown, or a page that did not open, is an
    /// `Err`. Free.
    pub(crate) fn step_library(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let shelf = match rest.trim() {
            "bookmarks" => super::library::Shelf::Bookmarks,
            "history" => super::library::Shelf::History,
            other => return Err(format!("`{other}` is not bookmarks or history")),
        };
        self.open_library(shelf, window, cx);
        if !self.library_open(cx) {
            return Err("the page did not open".into());
        }
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

    /// The four verbs below queue a script that only the in-memory page
    /// plays (`browser/pages.rs` hands the queue to `FakeWebBackend` and
    /// drops it for a real engine). On a real engine — a live `--steps`
    /// run — the verb would do nothing and report success, so it fails.
    fn scripted_page_only(&self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if self.browser.read(cx).fake || cfg!(all(test, feature = "probe")) {
            Ok(())
        } else {
            Err("only a capture's scripted page plays this; a real engine is showing".into())
        }
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
        self.scripted_page_only(cx)?;
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
        self.scripted_page_only(cx)?;
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
        self.scripted_page_only(cx)?;
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
        Ok(())
    }

    /// `crashed:<reason>`: script a renderer crash on the shown tab, so
    /// the `browse-crashed` capture shows the Baaz error page. A blank
    /// reason uses a default one; no shown tab is an `Err`. Free.
    pub(crate) fn step_crashed(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let reason = rest.trim();
        let reason = if reason.is_empty() { "the page crashed (-1)" } else { reason };
        self.scripted_page_only(cx)?;
        self.browser.update(cx, |browser, _| browser.fake_scripts.push(chrome::FakePageScript::Crashed(reason.to_owned())));
        if !self.rebuild_shown_browse_page(window, cx) {
            return Err("no tab is shown in Browse".into());
        }
        Ok(())
    }

    /// `page-actions` / `page-actions:<x>,<y>`: open the page controls' ⋯
    /// on the shown tab — the real one, the library's page menu
    /// (`BrowseMenuTarget::Page`) with the offers the page has, through the
    /// button's own path. It draws in the overlay window over the page (a
    /// capture lays that window over its shot, `shot.rs`). A click carries
    /// the button's bounds; `<x>,<y>` names the button's centre in window
    /// points, so the menu seats under it as a click seats it. With none,
    /// the menu opens at the pointer, wherever it stands (a capture's two
    /// shots then depend on the OS pointer). No shown page, or a point that
    /// does not parse, is an `Err`. Free.
    pub(crate) fn step_page_actions(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let anchor = match rest.trim() {
            "" => None,
            point => {
                let (x, y) = point
                    .split_once(',')
                    .and_then(|(x, y)| Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?)))
                    .ok_or_else(|| format!("`{point}` is not a window point x,y"))?;
                let side = cx.aui().metrics.control;
                Some(gpui::Bounds::centered_at(gpui::point(gpui::px(x), gpui::px(y)), gpui::size(side, side)))
            }
        };
        if self.browse_centre_page(window, cx).is_none() {
            return Err("no page is open in Browse".into());
        }
        self.toggle_browse_page_menu(anchor, window, cx);
        match self.overlays.read(cx).stack.browse_menu() {
            Some(menu) if matches!(menu.target, aui::screens::sidebar::BrowseMenuTarget::Page { .. }) => Ok(()),
            _ => Err("the page menu did not open".into()),
        }
    }

    /// `browse-ask:<text>`: the Field's Ask (its Ask row, ⌘↵) with `text`,
    /// through [`Self::browse_ask`] — the one Ask (docs/82 8.2): the line
    /// opens on the shown page and asks there. Only on scripted lanes
    /// (`--no-connect`, a capture): the ask goes to a browser chat whose
    /// lane sends nothing anywhere, so it reaches no model. Empty text, no
    /// page, live lanes, or a line that did not open fail the step.
    pub(crate) fn step_browse_ask(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let text = rest.trim();
        if text.is_empty() {
            return Err("browse-ask needs the question (`browse-ask:<text>`)".into());
        }
        if !crate::runtime::lanes_scripted() {
            return Err("browse-ask asks a browser chat: scripted lanes only (--no-connect)".into());
        }
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        if self.browse_centre_page(window, cx).is_none() {
            return Err("no page is open in Browse".into());
        }
        self.browse_ask(text, window, cx);
        if self.browser.read(cx).agent_window.line_ui == crate::app::agent_window::LineUi::Closed {
            return Err("the line did not open".into());
        }
        Ok(())
    }

    /// `shot-pick`: a page screenshot landing where the window stands
    /// ([`Self::attach_browser_screenshot`], what Screenshot reaches once
    /// the page answers) with a 1-px fixture picture, since the scripted
    /// page takes none. With no composer on screen (Browse, Settings, no
    /// session) "Add to which session?" opens; no picker (a composer on
    /// screen took it) fails the step. Free: nothing is attached until a
    /// pick.
    pub(crate) fn step_shot_pick(&mut self, _rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
            0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        self.attach_browser_screenshot(PNG.to_vec(), window, cx);
        if self.attach_picker_view(cx).is_none() {
            return Err("no picker opened: a composer on screen took the picture".into());
        }
        Ok(())
    }

    /// The tab a page verb acts on: Browse's shown tab, else the Page
    /// pane's.
    fn page_verb_tab(&self, cx: &gpui::App) -> Option<TabId> {
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.browser.read(cx).browse_centre_shown(cx)
        } else {
            self.pane_tab(cx)
        }
    }

    /// `keep-tab:<id|shown>`: Keep this tab on tab `id` (its number) or the
    /// one shown (Browse's, else the Page's) — the menus' own path. A tab
    /// that is no session's, or none, fails the step. Free.
    pub(crate) fn step_keep_tab(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.ensure_tabs_loaded(cx);
        let tab = match rest.trim() {
            "shown" | "" => self.page_verb_tab(cx).ok_or("no tab is shown")?,
            n => TabId(n.parse::<u64>().map_err(|_| format!("`{n}` is no tab id (`keep-tab:<id|shown>`)"))?),
        };
        if !self.keep_tab(tab, cx) {
            return Err(format!("tab {} is no session's tab", tab.0));
        }
        Ok(())
    }

    /// `page-expand`: the Page's "Open in Browse" — the session's tab full
    /// width in Browse, the same page (no reload). No session tab on the
    /// Page fails the step. Free.
    pub(crate) fn step_page_expand(&mut self, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.carry_session_tab_to_browse(window, cx);
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse || self.browser.read(cx).browse_session_tab.is_none() {
            return Err("no session tab on the Page to open in Browse".into());
        }
        Ok(())
    }

    /// `page-collapse`: "Back to the session's Page" from Browse showing a
    /// session's tab. Browse showing no project session's tab fails the
    /// step. Free.
    pub(crate) fn step_page_collapse(&mut self, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if !self.collapse_to_session_page(window, cx) {
            return Err("Browse shows no session's tab".into());
        }
        Ok(())
    }

    /// `send-page:<what>`: Send page (`what` empty) or selection
    /// (`selection`) to session… for
    /// the page shown (Browse's, else the Page's), through the composer
    /// router: the composer on screen takes it, else "Add to which
    /// session?" opens. Drafts only, never sends. No page (or no selection)
    /// fails the step. Free.
    pub(crate) fn step_send_page(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let selection = match rest.trim() {
            "" => false,
            "selection" => true,
            other => return Err(format!("`{other}`: `send-page` or `send-page:selection`")),
        };
        let tab = self.page_verb_tab(cx).ok_or("no page is shown")?;
        if !self.send_tab_page(tab, selection, window, cx) {
            return Err(if selection { "the page has no selection".into() } else { "the tab shows no page".into() });
        }
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
        // As "Open in Browse" does: the Field the entering ensure opened
        // goes, since the session's page is the one shown.
        self.leave_browse_address(window, cx);
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
