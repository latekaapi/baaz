//! The shown page's actions (task 4.5 split of `browser/mod.rs`): the ⋯
//! menu's items, back/forward/reload, navigation, and the history commands
//! (forget this site, clear history).

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    pub(crate) fn dismiss_browse_popovers(&mut self, cx: &mut Context<Self>) -> bool {
        use crate::overlay_stack::LayerKind;
        let open = self.overlays.update(cx, |o, cx| {
            o.close(LayerKind::Downloads, cx).is_some()
        });
        self.browser.update(cx, |browser, _| browser.downloads_rendered = false);
        if open {
            cx.notify();
        }
        open
    }

    /// Copy the shown tab's link (⇧⌘C in Browse, the ⋯ menu): the live
    /// page's URL, falling back to the tab's. A no-op with no shown tab
    /// or no URL.
    pub(crate) fn copy_shown_browse_link(&mut self, cx: &mut Context<Self>) {
        let url = self
            .browse_centre_state(cx)
            .map(|state| state.read(cx).url().to_string())
            .filter(|url| !url.is_empty())
            .or_else(|| {
                self.browser.read(cx).browse_centre_shown(cx).and_then(|id| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()))
            })
            .unwrap_or_default();
        if !url.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Open the shown tab's developer tools (⌥⌘I in Browse, the ⋯ menu).
    /// A no-op with no shown page, or where the backend cannot host them.
    pub(crate) fn open_browse_devtools(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.browse_centre_state(cx) {
            if state.read(cx).supports(aui_webview::BackendFeature::DevTools) {
                state.update(cx, |state, _| state.show_devtools());
            }
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Toggle annotate mode on the shown tab (the ⋯ menu): Browse hides
    /// the library's nav row, so this row is how the intent stays
    /// reachable. A no-op with no shown page.
    pub(crate) fn toggle_browse_annotate(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.browse_centre_state(cx) {
            let on = !state.read(cx).annotating();
            state.update(cx, |state, _| state.set_annotate(on));
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Screenshot the shown tab (the ⋯ menu, the Field's command): the
    /// same capture-or-pend path the library intent takes, read off the
    /// centre page instead of the pane's. Browse shows no composer, so the
    /// picture goes through "Add to which session?" (docs/82 8.2). The
    /// popover closes first, so the page it covered is visible in the
    /// capture; the pending capture keys to the shown tab, never the
    /// session key, so a switch can never misattribute it.
    pub(crate) fn capture_browse_screenshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_browse_popovers(cx);
        let shown = self.browser.read(cx).browse_centre_shown(cx);
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _cx| state.capture());
            match state.read(cx).screenshot().map(<[u8]>::to_vec) {
                Some(bytes) => self.attach_browser_screenshot(bytes, window, cx),
                None => {
                    if let Some(id) = shown {
                        self.browser.update(cx, |browser, _| browser.shot_pending.insert(screenshot_tab_key(id)));
                    }
                }
            }
        }
    }

    /// Whether the ⋯ menu offers "Open in Safari": only for `http(s)` pages.
    pub(crate) fn browse_safari_url(&self, cx: &gpui::App) -> Option<String> {
        let url = self
            .browse_centre_state(cx)
            .map(|state| state.read(cx).url().to_string())
            .unwrap_or_default();
        (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
    }

    /// Open the shown tab's URL in Safari (`open -a Safari <url>`).
    /// A no-op unless the URL is `http(s)`.
    pub(crate) fn open_browse_in_safari(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = self.browse_safari_url(cx) {
            let _ = crate::system_open::open_detached(["-a", "Safari", url.as_str()]);
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Open Settings at the Browser section (the ⋯ menu; the section
    /// exists, so no General fallback runs).
    pub(crate) fn open_browse_settings(&mut self, cx: &mut Context<Self>) {
        self.dismiss_browse_popovers(cx);
        self.open_settings_page("browser", cx);
    }

    /// "Site settings…" (the page menu, the Field, the site information):
    /// the shown page's site's own Settings page — what it may do, the
    /// agents' rule there, its zoom. A page with no site (`about:`, a
    /// file) opens Browser settings.
    pub(crate) fn open_shown_site_settings(&mut self, cx: &mut Context<Self>) {
        let url = self.browse_centre_state(cx).map(|state| state.read(cx).url().to_string()).unwrap_or_default();
        self.dismiss_browse_popovers(cx);
        match baaz_browser_core::sites::origin_of(&url) {
            Some(origin) => self.open_settings_page(&crate::settings::site_subpage(&origin), cx),
            None => self.open_settings_page("browser", cx),
        }
    }

    /// Reload the shown tab at its own URL (the crashed page's Reload):
    /// the navigation clears the crash, whatever the tab's owner.
    pub(crate) fn reload_browse_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else { return };
        let url = self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_default();
        if url.is_empty() {
            return;
        }
        let state = self.ensure_tab_page(HOME_KEY, id, window, cx);
        state.update(cx, |state, _cx| state.navigate(&url));
        cx.notify();
    }

    /// Navigate the Browse centre's shown tab to `url`: the model records
    /// it first so a discarded page rebuilds there, then the live page
    /// navigates when one exists. A navigation ends the address edit, like
    /// committing it does. Whether a load runs is the engine's own
    /// [`WebviewState::loading`](aui_webview::WebviewState::loading), never
    /// a Baaz-tracked set.
    pub(crate) fn navigate_browse_tab(
        &mut self,
        id: TabId,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_browse_address(window, cx);
        self.ensure_tabs_loaded(cx);
        let title = self.browser.read(cx).tabs.get(id).map(|tab| tab.title.clone()).unwrap_or_default();
        if self.browser.update(cx, |browser, _| browser.tabs.set_url_title(id, url, &title)) {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        match self.browser.read(cx).states.get(&id).cloned() {
            Some(state) => {
                if state.read(cx).url().as_ref() != url {
                    state.update(cx, |state, _cx| state.navigate(url));
                }
            }
            None => {
                self.ensure_tab_page(HOME_KEY, id, window, cx);
            }
        }
        cx.notify();
    }

    /// Step the shown tab back in its engine's own history: links, form
    /// posts, `pushState` and fragments are entries there, so a page that
    /// moved itself steps back the way a browser does. The tab's URL follows
    /// what the page reports, like any navigation.
    pub(crate) fn browse_go_back(&mut self, cx: &mut Context<Self>) {
        // The engine's answer (its new address) redraws the page.
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _| state.go_back());
        }
    }

    /// Step the shown tab forward in its engine's own history.
    pub(crate) fn browse_go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _| state.go_forward());
        }
    }

    /// "Never remember this site": the shown page's site leaves history
    /// (what was kept of it too) and is never recorded again. Settings ›
    /// Browser › Your data lists it, with a way back.
    pub(crate) fn forget_shown_site(&mut self, cx: &mut Context<Self>) {
        let url = self
            .browser.read(cx).browse_centre_shown(cx)
            .and_then(|id| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone()))
            .unwrap_or_default();
        let site = history::host_of(&url);
        if !history::recordable(&url) || site.is_empty() {
            self.overlays.update(cx, |o, cx| o.toast("Nothing to forget", "This page is not kept in history.", cx));
            return;
        }
        self.browser.read(cx).data.clone().update(cx, |data, cx| data.exclude(&site, cx));
        self.overlays.update(cx, |o, cx| {
            o.toast(format!("{site} won’t be remembered"), "Its history is gone. Undo in Settings › Browser.", cx)
        });
    }

    /// "Allow ads on this site": flips the shown site's exception; a toast
    /// says which way, and the page reloads so it takes effect.
    pub(crate) fn toggle_ads_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.browser.read(cx).browse_centre_shown(cx).and_then(|id| self.browser.read(cx).tabs.get(id).map(|tab| tab.url.clone())).unwrap_or_default();
        let site = history::host_of(&url);
        if !history::recordable(&url) || site.is_empty() {
            return;
        }
        let allowed = crate::layout::Layout::update(cx, |layout| layout.adblock.toggle(&site));
        adblock::apply(&crate::layout::Layout::get(cx).adblock);
        if let Some(id) = self.browser.read(cx).browse_centre_shown(cx) {
            self.reload_browse_tab(id, window, cx);
        }
        let (title, body) = if allowed {
            (format!("Ads allowed on {site}"), "Run the command again to block them.")
        } else {
            (format!("Ads blocked on {site}"), "")
        };
        self.overlays.update(cx, |o, cx| o.toast(title, body, cx));
    }

    /// Clear history: asks first, since it cannot be undone.
    pub(crate) fn ask_clear_history(&mut self, cx: &mut Context<Self>) {
        let pages = self.browser.read(cx).data.read(cx).history_len();
        self.set_dialog(
            cx,
            crate::overlays::Dialog {
                title: "Clear all browsing history?".to_owned(),
                detail: format!(
                    "{} and the words kept for search. Bookmarks, open tabs and sign-ins stay. This can’t be undone.",
                    if pages == 1 { "1 page".to_owned() } else { format!("{pages} pages") }
                ),
                kind: aui::overlay::DialogKind::Warning,
                primary: "Clear history",
                action: crate::overlays::DialogAction::ClearHistory,
                archive_target: None,
            },
        );
    }

    /// Clear history now (confirmed).
    pub(crate) fn clear_history_now(&mut self, cx: &mut Context<Self>) {
        self.browser.read(cx).data.clone().update(cx, |data, cx| data.clear_history(cx));
        self.overlays.update(cx, |o, cx| o.toast("History cleared", "", cx));
    }
}

/// Page zoom (docs/82 8.6): the page menu's Zoom row and ⌘= ⌘- ⌘0 set
/// the shown page's zoom and keep it for its site, beside the site's
/// permission answers in `browser.db`; a page reaching a site takes the
/// site's kept zoom (or 100 %) on arrival.
impl Harness {
    /// Step the shown page's zoom: `direction` +1 in, −1 out, 0 back to
    /// 100 %. Nothing on a page whose engine cannot zoom (rule 13: the
    /// row and keys only work where the engine does).
    pub(crate) fn zoom_shown_page(&mut self, direction: i8, cx: &mut Context<Self>) {
        let Some(state) = self.browse_centre_state(cx) else {
            return;
        };
        let (url, current, zooms) = {
            let page = state.read(cx);
            (page.url().to_string(), page.zoom(), page.supports(aui_webview::BackendFeature::Zoom))
        };
        if !zooms {
            return;
        }
        let origin = baaz_browser_core::sites::origin_of(&url);
        // The level asked last (kept at once) beats the engine's report,
        // which lands a poll later: two quick presses are two steps.
        let current = origin.as_deref().and_then(|origin| self.browser.read(cx).data.read(cx).sites().zoom(origin)).unwrap_or(current);
        let target = if direction == 0 { 100 } else { baaz_browser_core::sites::zoom_step(current, direction) };
        match origin {
            Some(origin) => self.set_site_zoom(&origin, target, cx),
            // The engine's answer redraws the page (its `Zoom` event).
            None => state.update(cx, |state, _| state.set_zoom(target)),
        }
    }

    /// Keep `percent` as `origin`'s zoom (100 forgets it) and zoom every
    /// open page on that site to it.
    pub(crate) fn set_site_zoom(&mut self, origin: &str, percent: u16, cx: &mut Context<Self>) {
        let value = (percent != 100).then(|| percent.to_string());
        let data = self.browser.read(cx).data.clone();
        data.update(cx, |data, cx| data.set_site(origin, baaz_browser_core::sites::ZOOM_KEY, value.as_deref(), cx));
        self.apply_site_zoom_to_open_pages(origin, percent, cx);
    }

    /// Zoom every open page on `origin` to `percent` (its kept zoom just
    /// changed, or was forgotten).
    pub(crate) fn apply_site_zoom_to_open_pages(&mut self, origin: &str, percent: u16, cx: &mut Context<Self>) {
        let pages: Vec<Entity<WebviewState>> = self.browser.read(cx).states.values().cloned().collect();
        for state in pages {
            let on_site = baaz_browser_core::sites::origin_of(&state.read(cx).url()).as_deref() == Some(origin);
            if on_site && state.read(cx).supports(aui_webview::BackendFeature::Zoom) && state.read(cx).zoom() != percent {
                state.update(cx, |state, _| state.set_zoom(percent));
            }
        }
    }

    /// The shown page's zoom for the page menu's Zoom row: `None` when its
    /// engine cannot zoom (the row does not show).
    pub(crate) fn shown_page_zoom(&self, cx: &gpui::App) -> Option<u16> {
        let state = self.browse_centre_state(cx)?;
        let page = state.read(cx);
        page.supports(aui_webview::BackendFeature::Zoom).then(|| page.zoom())
    }
}

impl BrowserView {
    /// Each live page that reached another site since the last pass takes
    /// that site's kept zoom, or 100 % (an engine such as WebKit keeps the
    /// last zoom across navigations, so a site with none must be put back).
    /// Once per arrival: within a site the person's own zoom stands.
    pub(super) fn zoom_new_sites(&mut self, live: &[(TabId, String, String, Option<String>)], cx: &mut Context<Self>) {
        self.zoomed_for.retain(|id, _| live.iter().any(|(live, ..)| live == id));
        for (id, url, _, _) in live {
            let Some(origin) = baaz_browser_core::sites::origin_of(url) else {
                continue;
            };
            if self.zoomed_for.get(id) == Some(&origin) {
                continue;
            }
            self.zoomed_for.insert(*id, origin.clone());
            let Some(state) = self.states.get(id).cloned() else {
                continue;
            };
            if !state.read(cx).supports(aui_webview::BackendFeature::Zoom) {
                continue;
            }
            let target = self.data.read(cx).sites().zoom(&origin).unwrap_or(100);
            if state.read(cx).zoom() != target {
                state.update(cx, |state, _| state.set_zoom(target));
            }
        }
    }
}

/// The Bookmarks and History page (docs/82 8.6, [`super::library`]).
impl Harness {
    /// Open the page on `shelf` over the Browse centre (entering Browse).
    pub(crate) fn open_library(&mut self, shelf: super::library::Shelf, window: &mut Window, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            self.set_mode(crate::layout::Mode::Browse, window, cx);
        }
        self.dismiss_browse_popovers(cx);
        if self.settings_open(cx) {
            self.close_settings(cx);
        }
        let Some(page) = self.browser.read(cx).library.clone() else {
            return;
        };
        // The page redraws itself; the root read it last frame (whether it
        // stands open), so the root's frame follows.
        page.update(cx, |page, cx| page.show(shelf, window, cx));
    }

    /// Shut the page, if it stands open.
    pub(crate) fn close_library(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = self.browser.read(cx).library.clone() {
            page.update(cx, |page, cx| page.close(cx));
        }
    }

    /// Whether the page stands open over the Browse centre.
    pub(crate) fn library_open(&self, cx: &gpui::App) -> bool {
        crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse
            && self.browser.read(cx).library.as_ref().is_some_and(|page| page.read(cx).is_open())
    }

    /// ⌘Y / ⇧⌘Y: the shelf, or shut it when it is the one shown.
    pub(crate) fn toggle_library(&mut self, shelf: super::library::Shelf, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.library_open(cx) && self.browser.read(cx).library.as_ref().is_some_and(|page| page.read(cx).shelf() == shelf);
        if showing {
            self.close_library(cx);
        } else {
            self.open_library(shelf, window, cx);
        }
    }

    pub(crate) fn on_library_event(&mut self, event: super::library::LibraryEvent, window: &mut Window, cx: &mut Context<Self>) {
        use super::library::LibraryEvent;
        match event {
            LibraryEvent::Close => self.close_library(cx),
            LibraryEvent::Open(url) => {
                self.close_library(cx);
                let _ = self.open_new_tab(&url, window, cx);
            }
            LibraryEvent::ClearHistory => self.ask_clear_history(cx),
        }
    }
}
