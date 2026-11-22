//! Pages and sessions, both ways (docs/82 8.3).
//!
//! - **Send page / selection to session…** from a tab's menu, the page's ⋯,
//!   the Field and the Page's nav row: the page's title and address, with
//!   its selected text quoted, go through the composer router
//!   ([`Harness::send_parcel`]) — the composer on screen, else "Add to which
//!   session?" and a toast with Open.
//! - **Keep this tab**: a session's tab becomes the person's through the one
//!   ownership move ([`tabs::Tabs::move_to`]), the same tab and live page.
//! - **One view at two sizes**: the Page's "Open in Browse" shows the
//!   session's tab full width ([`Harness::carry_session_tab_to_browse`]);
//!   "Back to the session's Page" returns it to the session's Page
//!   ([`Harness::collapse_to_session_page`]). The tab's one `WebviewState`
//!   moves between them, so the page never reloads.
//!
//! The selection is read from the page when a menu or the Field opens
//! ([`BrowserView::ask_selection`]); its answer lands on a later reconcile,
//! and the rows that need it show once it has.

use super::*;
use aui::screens::sidebar::MenuOffers;
use crate::layout::LayoutGlobal as _;

/// How much selected text a page sends.
const SELECTION_MAX: usize = 20_000;
/// When a selection read is collected for a page that does not report its
/// own answer (hidden): past a few of its 50 ms polls.
const SELECTION_COLLECT: std::time::Duration = std::time::Duration::from_millis(200);

/// The draft a page sends: its title and address, and the selection quoted
/// under them. Never sent; the person adds their words.
pub(crate) fn page_draft(title: &str, url: &str, selection: Option<&str>) -> String {
    let title = title.trim();
    let mut out = if title.is_empty() || title == url { url.to_owned() } else { format!("{title}\n{url}") };
    if let Some(selection) = selection.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n\n");
        out.push_str(&selection.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n"));
    }
    out
}

impl BrowserView {
    /// Ask `tab`'s live page for its selected text: the answer lands in
    /// [`Self::selections`] on a later reconcile (soon: one is armed). A tab
    /// with no live page has no selection.
    pub(crate) fn ask_selection(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let Some(state) = self.states.get(&tab).cloned() else {
            // Nothing redraws for this: the menu that asked reads it as it
            // draws.
            self.selections.remove(&tab);
            return;
        };
        let request = self.next_text_read;
        self.next_text_read += 1;
        self.selection_reads.insert(request, tab);
        state.update(cx, |state, _| state.eval_with_result(request, &aui_webview::agent_js::selection_text(SELECTION_MAX)));
        // A shown page's answer notifies and reconciles on its own; a hidden
        // one's (a background tab's menu) does not, so a reconcile is armed
        // past its next few polls to collect it.
        self.reconcile_after(SELECTION_COLLECT, cx);
    }

    /// A selection read answered: keep the text, or forget an empty one.
    pub(super) fn land_selection(&mut self, tab: TabId, answer: Result<String, String>, cx: &mut Context<Self>) {
        let text = answer
            .ok()
            .map(|text| serde_json::from_str::<String>(&text).unwrap_or(text))
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        let changed = match text {
            Some(text) => self.selections.insert(tab, text.clone()) != Some(text),
            None => self.selections.remove(&tab).is_some(),
        };
        if changed {
            cx.notify();
        }
    }
}

impl Harness {
    /// The tab the Page pane shows: the shown owner's active tab.
    pub(crate) fn pane_tab(&self, cx: &gpui::App) -> Option<TabId> {
        let owner = self.shown_tab_owner(cx);
        let space = self.browser.read(cx).owner_space(&owner);
        self.browser.read(cx).tabs.active_of(&owner, space.as_deref())
    }

    /// The session that owns `tab`, if one does.
    fn tab_session(&self, tab: TabId, cx: &gpui::App) -> Option<String> {
        match self.browser.read(cx).tabs.get(tab).map(|tab| &tab.owner) {
            Some(Owner::Session(session)) => Some(session.clone()),
            _ => None,
        }
    }

    /// The rows a menu on `tab` offers (rule 13: only what works): Send page
    /// for a tab on a page, Send selection while its page has text selected,
    /// Keep this tab for a session's tab.
    pub(crate) fn tab_offers(&self, tab: TabId, cx: &gpui::App) -> MenuOffers {
        let browser = self.browser.read(cx);
        let Some(found) = browser.tabs.get(tab) else { return MenuOffers::default() };
        let url = browser.states.get(&tab).map(|state| state.read(cx).url().to_string()).unwrap_or_else(|| found.url.clone());
        let page = !tabs::is_blank_url(&url);
        MenuOffers {
            send_page: page,
            send_selection: page && browser.selections.contains_key(&tab),
            keep: matches!(found.owner, Owner::Session(_)),
            collapse: false,
        }
    }

    /// What the page ⋯ and the Field offer for the page shown in Browse:
    /// [`Self::tab_offers`], plus "Back to the session's Page" when it is a
    /// project session's (a browser chat has no Page to go back to).
    pub(crate) fn shown_page_offers(&self, cx: &gpui::App) -> MenuOffers {
        let Some(tab) = self.browser.read(cx).browse_centre_shown(cx) else { return MenuOffers::default() };
        let mut offers = self.tab_offers(tab, cx);
        offers.collapse = self.tab_session(tab, cx).is_some_and(|session| !self.is_browser_chat(&session, cx));
        offers
    }

    /// The open Browse menu as it draws now: the stack's entry with its
    /// offers filled in from the current facts (a selection read lands after
    /// the menu opened, and its row appears then). What the sidebar, the
    /// overlay's size and its print read.
    pub(crate) fn browse_menu_now(&self, cx: &gpui::App) -> Option<aui::screens::sidebar::BrowseMenu> {
        use aui::screens::sidebar::BrowseMenuTarget;
        let mut menu = self.overlays.read(cx).stack.browse_menu().cloned()?;
        match &mut menu.target {
            BrowseMenuTarget::Tab { id, offers, .. } => {
                if let Some(tab) = crate::sidebar_view::browse_tab_id(id) {
                    *offers = self.tab_offers(tab, cx);
                }
            }
            BrowseMenuTarget::Page { offers, zoom, .. } => {
                *offers = self.shown_page_offers(cx);
                *zoom = self.shown_page_zoom(cx);
            }
            _ => {}
        }
        Some(menu)
    }

    /// Send `tab`'s page — with its selected text when `selection` — toward
    /// a composer ([`Self::send_parcel`]). Nothing when the tab shows no
    /// page, or `selection` is asked with none.
    pub(crate) fn send_tab_page(&mut self, tab: TabId, selection: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let offers = self.tab_offers(tab, cx);
        if !offers.send_page || (selection && !offers.send_selection) {
            return false;
        }
        let (title, url) = {
            let browser = self.browser.read(cx);
            let found = browser.tabs.get(tab);
            match browser.states.get(&tab) {
                Some(state) => (state.read(cx).title().to_string(), state.read(cx).url().to_string()),
                None => found.map(|t| (t.title.clone(), t.url.clone())).unwrap_or_default(),
            }
        };
        let selection = if selection { self.browser.read(cx).selections.get(&tab).cloned() } else { None };
        self.send_parcel(Parcel::Page { title, url, selection }, window, cx);
        true
    }

    /// Keep this tab: a session's tab becomes the person's, in the current
    /// space and out of the task folder — the same tab id and live page, so
    /// nothing reloads, and the move persists. The session's agent no longer
    /// reaches it; the session's Page falls back to its next tab (or, with
    /// none, a fresh blank one when it next shows). Browse showing it as the
    /// session's tab goes on showing it, as the person's. False for a tab
    /// that is not a session's.
    pub(crate) fn keep_tab(&mut self, tab: TabId, cx: &mut Context<Self>) -> bool {
        self.ensure_tabs_loaded(cx);
        if self.tab_session(tab, cx).is_none() {
            return false;
        }
        let space = self.browser.read(cx).browse_space();
        if !self.browser.update(cx, |browser, _| browser.tabs.move_to(tab, Owner::Person, space.as_deref())) {
            return false;
        }
        // The page is the person's now: the agent's routes to it go.
        if let Some(state) = self.browser.read(cx).states.get(&tab).cloned() {
            self.terminal_service.register_tab_page(HOME_KEY, tab.0, state);
        }
        self.browser.update(cx, |browser, _| browser.agent_tabs.retain(|_, id| *id != tab));
        if self.browser.read(cx).browse_session_tab == Some(tab) {
            self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        }
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        self.overlays.update(cx, |o, cx| o.toast("Kept as your tab", "It is in your tabs in Browse now.", cx));
        true
    }

    /// Back to the session's Page: Browse showing a project session's tab
    /// returns to Sessions with that session open on its Page, on this tab.
    /// The tab's one page moves back — no reload. False when Browse shows
    /// no session's tab (or a browser chat's, which has no Page).
    pub(crate) fn collapse_to_session_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return false;
        }
        let Some(tab) = self.browser.read(cx).browse_centre_shown(cx) else { return false };
        let Some(session) = self.tab_session(tab, cx).filter(|session| !self.is_browser_chat(session, cx)) else { return false };
        self.browser.update(cx, |browser, _| browser.tabs.activate(tab));
        self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        // The session's pane opens on its Page wherever the landing restores
        // it from: now, or when an async resume lands.
        self.set_override(
            &session,
            |meta| {
                let mut right = meta.right.clone().unwrap_or_default();
                right.open = true;
                right.kind = crate::layout::RightKind::Browser;
                meta.right = Some(right);
            },
            cx,
        );
        self.set_mode(crate::layout::Mode::Sessions, window, cx);
        if self.active_id(cx).as_deref() == Some(session.as_str()) {
            self.open_right_on(crate::layout::RightKind::Browser, cx);
        } else {
            self.resume(session, window, cx);
        }
        true
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::page_draft;

    #[test]
    fn a_page_sends_its_title_and_address_and_quotes_the_selection() {
        assert_eq!(page_draft("Pricing · Acme", "https://acme.example/pricing", None), "Pricing · Acme\nhttps://acme.example/pricing");
        assert_eq!(page_draft("", "https://acme.example/", None), "https://acme.example/", "no title: the address alone");
        assert_eq!(page_draft("https://a.example/", "https://a.example/", None), "https://a.example/", "a title that is the address is said once");
        assert_eq!(
            page_draft("Pricing", "https://acme.example/pricing", Some("Team $24 / seat\nSSH + cloud VMs")),
            "Pricing\nhttps://acme.example/pricing\n\n> Team $24 / seat\n> SSH + cloud VMs"
        );
        assert_eq!(page_draft("P", "https://p.example/", Some("   ")), "P\nhttps://p.example/", "blank selection sends the page alone");
    }
}
