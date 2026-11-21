//! Downloads and the extensions popover (task 4.5 split of
//! `browser/mod.rs`).

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    /// Dismiss every Browse chrome popover (downloads, extensions):
    /// row activation, the toggle press, click-outside, Escape and tab
    /// switches all land here. Answers whether one stood open.
    /// The sidebar's downloads button (D4): opens the downloads popover
    /// under it, or closes it.
    pub(crate) fn toggle_browse_downloads(&mut self, anchor: Option<gpui::Bounds<gpui::Pixels>>, cx: &mut Context<Self>) {
        let open = !self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::Downloads);
        self.dismiss_browse_popovers(cx);
        self.overlays.update(cx, |o, cx| o.set(crate::overlay_stack::Layer::Downloads, open, cx));
        self.browser.update(cx, |browser, _| browser.downloads_anchor = anchor);
    }

    /// The footer's extensions button (R4): its popover above it.
    pub(crate) fn toggle_browse_extensions(&mut self, anchor: Option<gpui::Bounds<gpui::Pixels>>, cx: &mut Context<Self>) {
        let open = !self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::Extensions);
        self.dismiss_browse_popovers(cx);
        self.overlays.update(cx, |o, cx| o.set(crate::overlay_stack::Layer::Extensions, open, cx));
        self.browser.update(cx, |browser, _| browser.downloads_anchor = anchor);
    }

    /// Cancel the download `id` on its owning tab's page.
    pub(crate) fn cancel_browse_download(&mut self, tab: TabId, id: u32, cx: &mut Context<Self>) {
        if let Some(state) = self.browser.read(cx).states.get(&tab).cloned() {
            state.update(cx, |state, _| state.cancel_download(id));
        }
        cx.notify();
    }

    /// Retry a failed download (its "Try again"): re-navigate the owning
    /// tab to the download's URL, exactly as re-clicking the link would.
    pub(crate) fn retry_browse_download(&mut self, tab: TabId, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.read(cx).tabs.get(tab).is_none() {
            return;
        }
        let state = self.ensure_tab_page(HOME_KEY, tab, window, cx);
        state.update(cx, |state, _cx| state.navigate(url));
        self.dismiss_browse_popovers(cx);
    }

    /// Reveal a landed file in Finder (`open -R -- <path>`). A file that
    /// moved or vanished toasts instead of revealing nothing.
    pub(crate) fn reveal_browse_download(&mut self, path: &str, cx: &mut Context<Self>) {
        if !std::path::Path::new(path).exists() {
            self.overlays.update(cx, |overlays, cx| overlays.toast("Downloads", "File moved or deleted", cx));
            return;
        }
        let _ = crate::system_open::open_detached([std::ffi::OsStr::new("-R"), std::ffi::OsStr::new("--"), path.as_ref()]);
    }
}

impl BrowserView {

    /// Open the downloads folder ("All downloads", `~/Downloads`, resolved
    /// from `$HOME` with no `dirs` dependency). With no `$HOME` there is
    /// nothing to open: log and return.
    pub(crate) fn open_downloads_folder() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            crate::baaz_log!("downloads: no $HOME, cannot open the downloads folder");
            return;
        };
        let _ = crate::system_open::open_detached([home.join("Downloads")]);
    }


    /// Every download's latest state across the live tabs, with the tab
    /// that owns it (cancels and "saved by" labels route through it).
    pub(crate) fn browse_downloads(&self, cx: &gpui::App) -> Vec<(TabId, Owner, DownloadState)> {
        let mut out = Vec::new();
        for (id, state) in &self.states {
            let owner = self.tabs.get(*id).map(|tab| tab.owner.clone()).unwrap_or(Owner::Person);
            for download in state.read(cx).downloads() {
                out.push((*id, owner.clone(), download.clone()));
            }
        }
        out.sort_by_key(|(_, _, download)| download.id);
        out
    }

    /// Stamp newly seen active downloads and prune ended ones, once per
    /// frame. The popover's "6 s left" reads the elapsed time since the
    /// stamp against the average rate so far.
    pub(super) fn stamp_browse_downloads(&mut self, cx: &mut gpui::App) {
        let now = tabs::now_ms();
        let mut live = std::collections::HashSet::new();
        for (id, state) in &self.states {
            for download in state.read(cx).downloads() {
                if download.state == aui_webview::DownloadStatus::Downloading {
                    live.insert((*id, download.id));
                    self.download_started.entry((*id, download.id)).or_insert(now);
                }
            }
        }
        self.download_started.retain(|key, _| live.contains(key));
    }

    /// Whether a footer popover (downloads or extensions, R4) wants its
    /// overlay window: opened from its button. An empty downloads list
    /// still opens (it says so).
    pub(crate) fn downloads_overlay_wanted(&self, cx: &gpui::App) -> bool {
        crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse
            && (self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::Downloads) || self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::Extensions))
            && self.downloads_anchor.is_some()
    }
}
