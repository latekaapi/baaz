//! Switching modes keeps your place (docs/82 8.4): in Browse the sidebar's
//! footer says what waits on the person ("Needs you" with its count) and
//! names the session Sessions stood on; in Sessions a session that owns
//! pages says "N pages" on its row and in its header, and the click opens
//! them.
//!
//! "The session Sessions stood on" is the open session: a mode switch
//! leaves it open ([`Harness::set_mode`]), so Browse names it and the way
//! back lands on it. With nothing open it is the newest listed session.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

use aui::screens::sidebar::LastSession;

use crate::browser::Owner;
use crate::sidebar::SessionRow as _;

/// What the pages of a session open on: its Page in the pane when it owns
/// one, Browse on its task folder when it owns several (the Page shows one
/// page at a time and has no list of its own; the task folder lists them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PagesOpen {
    /// One page: Sessions, the session open, its Page in the pane.
    Page(crate::browser::TabId),
    /// Several: Browse on the session's own tab, its task folder listing
    /// every page.
    Browse(crate::browser::TabId),
}

/// Where a session's pages open, from what it owns (`tabs`, strip order)
/// and which of them is its active one. `None` when it owns none.
pub(crate) fn pages_open(tabs: &[crate::browser::TabId], active: Option<crate::browser::TabId>) -> Option<PagesOpen> {
    let first = *tabs.first()?;
    let shown = active.filter(|id| tabs.contains(id)).unwrap_or(first);
    Some(if tabs.len() == 1 { PagesOpen::Page(shown) } else { PagesOpen::Browse(shown) })
}

impl Harness {
    /// Browse's footer facts (docs/82 8.4): how many sessions wait on the
    /// person (the Sessions nav row's own count) and the session Sessions
    /// stood on — the open one (its chain head, as its row names it), else
    /// the newest listed one — with its row's state mark.
    pub(crate) fn browse_context(&self, cx: &gpui::App) -> (u32, Option<LastSession>) {
        let visible = self.visible_sessions(cx);
        let needs_you = crate::attention::waiting(visible.iter()).len() as u32;
        let open = self.active_id(cx).map(|id| self.chain_head(&id, cx));
        let last = match open {
            Some(head) => {
                let entry = visible.iter().find(|entry| entry.id == head);
                let title = self.collapsed_head_label(&head, cx).unwrap_or_else(|| crate::sidebar::UNNAMED.to_owned());
                Some(last_session(head, title, entry))
            }
            None => visible.first().map(|entry| last_session(entry.id.clone(), entry.label.clone(), Some(entry))),
        };
        (needs_you, last)
    }

    /// The Browse footer's "Needs you": back to Sessions showing only what
    /// waits (the nav row's filter, turned on) with the first waiting
    /// session open — the sidebar's order, as ⌘⇧J walks it. Nothing waiting
    /// does nothing.
    pub(crate) fn back_to_needs_you(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_sessions(cx);
        let order: Vec<String> = crate::attention::waiting(visible.iter()).into_iter().map(|row| row.id.clone()).collect();
        let Some(first) = crate::attention::next_waiting(&order, None) else { return };
        if !crate::attention::Notifier::filter_on(cx) {
            self.toggle_needs_you_filter(cx);
        }
        self.set_mode(crate::layout::Mode::Sessions, window, cx);
        if self.active_id(cx).map(|id| self.chain_head(&id, cx)).as_deref() != Some(first.as_str()) {
            self.resume_quiet(first, window, cx);
        }
    }

    /// The Browse footer's last session: back to Sessions on it — it is
    /// still open when it was the open one, so only the mode moves.
    pub(crate) fn back_to_session(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Sessions, window, cx);
        if self.active_id(cx).map(|id| self.chain_head(&id, cx)).as_deref() != Some(session_id.as_str()) {
            self.resume_quiet(session_id, window, cx);
        }
    }

    /// The pages `session_id` owns, in strip order, and its active one.
    fn session_pages(&self, session_id: &str, cx: &gpui::App) -> (Vec<crate::browser::TabId>, Option<crate::browser::TabId>) {
        let browser = self.browser.read(cx);
        let owner = Owner::Session(session_id.to_owned());
        let tabs = browser.tabs.ordered_tabs(&owner, None).iter().map(|tab| tab.id).collect();
        (tabs, browser.tabs.active_of(&owner, None))
    }

    /// "N pages" on a row or in the header (docs/82 8.4): one page opens
    /// on the session's Page (Sessions, the session open, the pane on its
    /// Page on that tab); several open Browse on the session's own tab,
    /// where its task folder lists every page. False when it owns none.
    pub(crate) fn open_session_pages(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_tabs_loaded(cx);
        let (tabs, active) = self.session_pages(&session_id, cx);
        match pages_open(&tabs, active) {
            None => false,
            Some(PagesOpen::Page(tab)) => {
                self.browser.update(cx, |browser, _| browser.tabs.activate(tab));
                self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
                    self.set_mode(crate::layout::Mode::Sessions, window, cx);
                }
                if self.active_id(cx).as_deref() == Some(session_id.as_str()) {
                    self.open_right_on(crate::layout::RightKind::Browser, cx);
                } else {
                    // The pane opens on the Page wherever the landing restores
                    // it from (as the row's "N running" does for Tasks).
                    let mut state = self
                        .session_list
                        .read(cx)
                        .overrides
                        .get(&session_id)
                        .and_then(|meta| meta.right.clone())
                        .unwrap_or_default();
                    state.open = true;
                    state.kind = crate::layout::RightKind::Browser;
                    self.store_right_state_cheap(&session_id, state, cx);
                    self.resume_quiet(session_id, window, cx);
                    self.open_right_on(crate::layout::RightKind::Browser, cx);
                }
                true
            }
            Some(PagesOpen::Browse(tab)) => {
                self.set_mode(crate::layout::Mode::Browse, window, cx);
                self.browse_sidebar_tab(tab, false, window, cx);
                true
            }
        }
    }

    /// The open session's page count, for the header's "N pages" (zero
    /// with no session open or no pages).
    pub(crate) fn active_page_count(&self, cx: &gpui::App) -> u32 {
        let Some(id) = self.active_id(cx) else { return 0 };
        let browser = self.browser.read(cx);
        if !browser.tabs_loaded {
            return 0;
        }
        browser.tabs.tabs_of(&Owner::Session(id)).len() as u32
    }

    #[cfg(feature = "probe")]
    /// `pages-fixture` (or `pages-fixture:<session_id>[,<n>]`): capture aid — `n` pages (2 by
    /// default, at most 9) owned by that sidebar row (the open session with
    /// no id), as tabs of its own on fixed `example.com` paths; no page is
    /// loaded. No such row (or no session open), a bad count, or a session
    /// that already owns pages fails the step.
    pub(crate) fn step_pages_fixture(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (named, count) = match rest.split_once(',') {
            Some((id, n)) => (id.trim(), n.trim().parse::<u32>().map_err(|_| format!("pages-fixture: `{n}` is not a count"))?),
            None => (rest.trim(), 2),
        };
        if !(1..=9).contains(&count) {
            return Err(format!("pages-fixture takes 1 to 9 pages, not {count}"));
        }
        let id = if named.is_empty() {
            self.active_id(cx).ok_or("pages-fixture: no session is open and none was named")?
        } else if self.session_list.read(cx).sessions.iter().any(|entry| entry.id == named) {
            named.to_owned()
        } else {
            return Err(format!("no session row `{named}`"));
        };
        let id = id.as_str();
        self.ensure_tabs_loaded(cx);
        let owner = Owner::Session(id.to_owned());
        if !self.browser.read(cx).tabs.tabs_of(&owner).is_empty() {
            return Err(format!("`{id}` already owns pages"));
        }
        for n in 1..=count {
            let url = format!("https://example.com/page-{n}");
            self.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), &url, None));
        }
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `pages-row:<session_id>`: the row's "N pages" click. A session that
    /// owns no pages fails the step.
    pub(crate) fn step_pages_row(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        if !self.open_session_pages(id.to_owned(), window, cx) {
            return Err(format!("`{id}` owns no pages"));
        }
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `browse-footer:<needs-you|last>`: the Browse footer's row click.
    /// Outside Browse, or a row the footer does not show, fails the step.
    pub(crate) fn step_browse_footer(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return Err("browse-footer: not in Browse".into());
        }
        let (needs_you, last) = self.browse_context(cx);
        match rest.trim() {
            "needs-you" if needs_you > 0 => self.back_to_needs_you(window, cx),
            "needs-you" => return Err("browse-footer: nothing waits on you".into()),
            "last" => match last {
                Some(last) => self.back_to_session(last.id.to_string(), window, cx),
                None => return Err("browse-footer: no session to go back to".into()),
            },
            other => return Err(format!("browse-footer takes needs-you or last, not `{other}`")),
        }
        Ok(())
    }
}

/// The footer's last-session row for `id`, its words and its row's state.
fn last_session(id: String, title: String, entry: Option<&SessionEntry>) -> LastSession {
    let (state, unread) = entry.map_or((aui_tokens::AgentState::Idle, false), |entry| {
        (entry.state(), entry.session_state() == crate::sidebar::SessionState::DoneUnread)
    });
    LastSession { id: id.into(), title: title.into(), state, unread }
}

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::{pages_open, PagesOpen};
    use crate::browser::TabId;

    #[test]
    fn one_page_opens_the_page_and_several_open_browse_on_the_active_one() {
        let (a, b) = (TabId(1), TabId(2));
        assert_eq!(pages_open(&[], None), None, "no pages: nothing to open");
        assert_eq!(pages_open(&[a], None), Some(PagesOpen::Page(a)));
        assert_eq!(pages_open(&[a, b], Some(b)), Some(PagesOpen::Browse(b)), "the session's active page");
        assert_eq!(pages_open(&[a, b], None), Some(PagesOpen::Browse(a)), "else its first");
        assert_eq!(pages_open(&[a, b], Some(TabId(9))), Some(PagesOpen::Browse(a)), "an active id it no longer owns is ignored");
    }
}
