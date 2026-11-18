//! The shown page's actions (task 4.5 split of `browser/mod.rs`): the ⋯
//! menu's items, back/forward/reload, navigation, and the history commands
//! (forget this site, clear history).

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    pub(crate) fn dismiss_browse_popovers(&mut self, cx: &mut Context<Self>) -> bool {
        use crate::overlay_stack::LayerKind;
        let open = self.overlays.update(cx, |o, cx| {
            // Close all three: no short circuit.
            [LayerKind::Downloads, LayerKind::Extensions, LayerKind::More].map(|kind| o.close(kind, cx).is_some()).contains(&true)
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

    /// Step the shown tab's zoom along Chrome's ladder (the ⋯ menu's –/+
    /// row): `delta` is a direction, not points — positive climbs one
    /// rung, negative drops one, zero holds.
    pub(crate) fn browse_zoom_step(&mut self, delta: i16, cx: &mut Context<Self>) {
        if delta == 0 {
            return;
        }
        if let Some(state) = self.browse_centre_state(cx) {
            let next = toolbar::zoom_step(state.read(cx).zoom(), delta > 0);
            state.update(cx, |state, _| state.set_zoom(next));
        }
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

    /// Screenshot the shown tab into the active session's draft (the ⋯
    /// menu): the same capture-or-pend path the library intent takes, read
    /// off the centre page instead of the pane's. The popover closes
    /// first, so the page it covered is visible in the capture; the
    /// pending capture keys to the shown tab, never the session key, so a
    /// switch can never misattribute it.
    pub(crate) fn capture_browse_screenshot(&mut self, cx: &mut Context<Self>) {
        self.dismiss_browse_popovers(cx);
        let shown = self.browser.read(cx).browse_centre_shown(cx);
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _cx| state.capture());
            match state.read(cx).screenshot().map(<[u8]>::to_vec) {
                Some(bytes) => self.attach_browser_screenshot(bytes, cx),
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

    /// Step the shown tab back within its Baaz-tracked history.
    pub(crate) fn browse_go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else {
            return;
        };
        let url = self.browser.update(cx, |browser, _| browser.histories.entry(id).or_default().go_back());
        if let Some(url) = url {
            self.navigate_browse_tab(id, &url, window, cx);
        }
    }

    /// Step the shown tab forward within its Baaz-tracked history.
    pub(crate) fn browse_go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).browse_centre_shown(cx) else {
            return;
        };
        let url = self.browser.update(cx, |browser, _| browser.histories.entry(id).or_default().go_forward());
        if let Some(url) = url {
            self.navigate_browse_tab(id, &url, window, cx);
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
