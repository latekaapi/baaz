//! D6's Page screen in Baaz (docs/43 §2.7): the session's one page under its
//! 36 px nav row; "No page yet" with an address field before the agent (or
//! the person) opens one; Baaz's own "This page couldn’t load" over a failed
//! or crashed page. The pill and the notes panel float over the native page
//! in their own window (`overlay.rs`, `OverlayKind::PageFloat`).

use aui::workbench::{address_field_frame, no_page_state, page_error_state, recent_pages_menu, state_block, PaneRow, RecentRow};
use aui_webview::WebviewState;
use aui_icons::IconName;
use aui_tokens::ActiveAui;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, IntoElement, SharedString, Window};
use gpui_kit::base::input::{Input, InputEvent, InputState};

use crate::app::Harness;
use crate::right::BrowserPane;

/// A page with nothing in it yet.
pub(crate) fn is_blank(url: &str) -> bool {
    matches!(url.trim(), "" | "about:blank")
}

/// What the engine's error means, as the error page says it after the host.
pub(crate) fn load_failure_words(reason: &str) -> String {
    let code = reason.trim().trim_start_matches("net::");
    match code {
        "ERR_CONNECTION_REFUSED" => "refused the connection.".into(),
        "ERR_NAME_NOT_RESOLVED" => "could not be found.".into(),
        "ERR_INTERNET_DISCONNECTED" => "is out of reach: this Mac is offline.".into(),
        "ERR_CONNECTION_TIMED_OUT" | "ERR_TIMED_OUT" => "took too long to answer.".into(),
        "ERR_CONNECTION_RESET" | "ERR_CONNECTION_CLOSED" | "ERR_EMPTY_RESPONSE" => "closed the connection.".into(),
        "ERR_CERT_AUTHORITY_INVALID" | "ERR_CERT_COMMON_NAME_INVALID" | "ERR_CERT_DATE_INVALID" => "has a certificate Baaz does not trust.".into(),
        other => format!("did not load ({other})."),
    }
}

/// The host part of an address, for the error page.
fn host_of(url: &str) -> String {
    aui::workbench::split_url(url).0.to_string()
}

impl Harness {
    /// The no-page address field: made once, Enter opens what was typed (an
    /// address, or a search with the person's engine) as the session's page.
    pub(crate) fn ensure_page_address(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.right_cache.page_address {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter an address"));
        cx.subscribe(&input, |this: &mut Self, input, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let text = input.read(cx).value().to_string();
                let engine = this.layout.search_engine;
                let url = match crate::browser::toolbar::classify_address_input_with(&text, engine) {
                    crate::browser::toolbar::NavTarget::Url(url) => url,
                    crate::browser::toolbar::NavTarget::Search(q) if q.is_empty() => return,
                    crate::browser::toolbar::NavTarget::Search(q) => engine.search_url(&q),
                };
                this.open_url_in_browser(url, cx);
            }
        })
        .detach();
        self.right_cache.page_address = Some(input.clone());
        input
    }

    /// Open or close the Page row's Recent pages menu.
    pub(crate) fn set_page_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.right_cache.page_menu != open {
            self.right_cache.page_menu = open;
            cx.notify();
        }
    }

    /// A Recent pages row: the session's one page goes there; the menu closes.
    pub(crate) fn open_recent_page(&mut self, index: usize, cx: &mut Context<Self>) {
        self.set_page_menu(false, cx);
        let Some(state) = self.existing_browser(cx) else { return };
        let url = state.read(cx).recent_pages().get(index).map(|p| p.url.to_string());
        if let Some(url) = url {
            state.update(cx, |s, _| s.navigate(&url));
        }
    }

    /// Whether the Page screen draws its own state over the page (no page
    /// yet, couldn't load, crashed, a pushed view) — the native view hides
    /// under it.
    pub(crate) fn pane_page_covered(&self, cx: &gpui::App) -> bool {
        self.existing_browser(cx).is_some_and(|state| page_covered(state.read(cx), &self.right_cache))
    }
}

/// Whether the Page screen draws over the page, so the native view hides:
/// no page yet, couldn't load, crashed, or a full output / image pushed on
/// the Page screen (live 2026-10-19: the page drew over the output view).
pub(crate) fn page_covered(state: &WebviewState, cache: &crate::right::RightCache) -> bool {
    let page_screen = crate::right::screen_index(crate::layout::RightKind::Browser);
    cache.pushed.contains_key(&page_screen) || is_blank(&state.url()) || state.load_failed().is_some() || state.crashed().is_some()
}

/// The Recent pages rows for `state`: newest first, the page on screen checked.
pub(crate) fn recent_rows(state: &WebviewState) -> Vec<RecentRow> {
    let current = state.url();
    state.recent_pages().iter().map(|p| RecentRow::new(&p.url, &p.title, p.url == current)).collect()
}

/// The Page row: its title is the Recent pages button once the session's
/// page has shown an address (owner 2026-10-18, canvas v50 `D6PageRecent`).
pub(crate) fn page_row(row: PaneRow, open: bool, state: Option<&Entity<WebviewState>>, cx: &mut Context<Harness>) -> PaneRow {
    let has = state.is_some_and(|s| !s.read(cx).recent_pages().is_empty());
    if !has {
        return row;
    }
    let row = row.title_menu(open, cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| {
        let open = !this.right_cache.page_menu;
        this.set_page_menu(open, cx);
    }));
    row
}

/// Whether the Page screen shows a page (not the "No page yet" state): then
/// its nav row is the screen's one bar and the pane row goes.
pub(crate) fn shows_page(state: Option<&Entity<WebviewState>>, cx: &gpui::App) -> bool {
    state.is_some_and(|s| !is_blank(&s.read(cx).url()))
}

/// Where the Recent pages menu hangs, inside the screen: under the bar from
/// the address field's left edge when the bar is merged, else from the old
/// pane row's title.
pub(crate) fn recent_menu_seat(merged: bool, width: f32) -> (f32, f32) {
    if merged {
        (aui::workbench::page_address_left(width), aui::workbench::RECENT_MENU_TOP)
    } else {
        (6.0, 2.0)
    }
}

/// The pane width the menu sizes itself from, so that from its seat it ends
/// 6 in from the pane's right edge (the menu is `min(320, w − 12)` wide).
/// Live 2026-10-21: hung from the address field at full width it ran past
/// the pane's edge and lost its right corner.
pub(crate) fn recent_menu_room(merged: bool, width: f32) -> f32 {
    let (left, _) = recent_menu_seat(merged, width);
    width - left + 6.0
}

/// The menu drawn inside the window, for a page that is not a native view
/// (the scripted page captures and tests run); a native page gets it in its
/// overlay window ([`crate::overlay`]), since it draws above gpui.
pub(crate) fn page_menu_in_window(open: bool, state: Option<&Entity<WebviewState>>, cache: &crate::right::RightCache, width: f32, merged: bool, cx: &mut Context<Harness>) -> Option<AnyElement> {
    let state = state?;
    // In the window unless a live native page is showing (that one gets the
    // overlay window): a covered page — its error page, say — hides it.
    let native_showing = state.read(cx).is_native() && !page_covered(state.read(cx), cache);
    if !open || native_showing {
        return None;
    }
    let menu = recent_menu(&recent_rows(state.read(cx)), recent_menu_room(merged, width), cx);
    let close = cx.listener(|this: &mut Harness, _: &gpui::MouseDownEvent, _, cx| this.set_page_menu(false, cx));
    let (left, top) = recent_menu_seat(merged, width);
    Some(div().absolute().left(px(left)).top(px(top)).on_mouse_down_out(close).child(menu).into_any_element())
}

/// The menu itself, its rows wired to the session's page.
pub(crate) fn recent_menu(rows: &[RecentRow], width: f32, cx: &mut Context<Harness>) -> AnyElement {
    let weak = cx.weak_entity();
    let pick = std::rc::Rc::new(move |index: usize, _: &mut Window, cx: &mut gpui::App| {
        let _ = weak.update(cx, |this, cx| this.open_recent_page(index, cx));
    });
    recent_pages_menu("right-page-recent", rows, width, pick).into_any_element()
}

/// The Page screen's body.
pub(crate) fn page_body(browser: BrowserPane<'_>, pane_open: bool, agent: &str, address: Option<Entity<InputState>>, menu_open: bool, cx: &mut Context<Harness>) -> AnyElement {
    let Some(state) = browser.state else {
        return if pane_open {
            state_block("right-browser-loading", IconName::Globe, "Opening the page").into_any_element()
        } else {
            div().size_full().into_any_element()
        };
    };
    let p = cx.aui().colors;
    let (url, failed, crashed) = {
        let s = state.read(cx);
        (s.url().to_string(), s.load_failed(), s.crashed())
    };
    if is_blank(&url) {
        let field = address_field_frame(false, cx).child(match address {
            Some(input) => div().flex_1().min_w(px(0.0)).child(Input::new(&input)).into_any_element(),
            None => div().text_color(p.fg_muted).child("Search or enter an address").into_any_element(),
        });
        return no_page_state("right-page-none", field, agent, cx).into_any_element();
    }
    let weak = cx.weak_entity();
    let has_recent = !state.read(cx).recent_pages().is_empty();
    let mut pane = aui_webview::webview_pane("right-browser", state)
        .agent(SharedString::from(agent.to_string()))
        .on_intent(move |intent, window, cx| {
            let _ = weak.update(cx, |harness, cx| harness.handle_browser_intent(intent, window, cx));
        })
        .on_close({
            let weak = cx.weak_entity();
            move |_, cx| {
                let _ = weak.update(cx, |this, cx| this.toggle_right(cx));
            }
        });
    if has_recent {
        let weak = cx.weak_entity();
        pane = pane.recent_pages(menu_open, move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                let open = !this.right_cache.page_menu;
                this.set_page_menu(open, cx);
            });
        });
    }
    let pane = pane.into_any_element();
    // Couldn't load / crashed: Baaz's own page over the page area, under the
    // nav row, so another address is one field away (canvas D6StatesPage;
    // live 2026-10-19 the row went with the page).
    let error = if let Some((failed_url, reason)) = failed {
        let retry_state = state.clone();
        let retry_url = failed_url.to_string();
        Some(page_error_state("right-page-error", &host_of(&failed_url), &load_failure_words(&reason), move |_, _, cx| {
            let url = retry_url.clone();
            retry_state.update(cx, |s, _| s.navigate(&url));
        }, cx))
    } else {
        crashed.map(|reason| {
            let retry_state = state.clone();
            let retry_url = url.clone();
            page_error_state("right-page-crashed", &host_of(&url), &format!("stopped: {reason}."), move |_, _, cx| {
                let url = retry_url.clone();
                retry_state.update(cx, |s, _| s.navigate(&url));
            }, cx)
        })
    };
    match error {
        Some(error) => div()
            .relative()
            .size_full()
            .child(pane)
            .child(
                div()
                    .absolute()
                    .top(px(aui::workbench::PAGE_NAV_H))
                    .bottom(px(0.0))
                    .left(px(0.0))
                    .right(px(0.0))
                    .bg(p.surface_2)
                    .child(error),
            )
            .into_any_element(),
        None => pane,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_failures_read_as_sentences() {
        assert_eq!(load_failure_words("net::ERR_CONNECTION_REFUSED"), "refused the connection.");
        assert_eq!(load_failure_words("net::ERR_SOMETHING"), "did not load (ERR_SOMETHING).");
        assert!(is_blank("about:blank") && is_blank(" ") && !is_blank("https://x"));
    }

    /// The menu hangs from the address field and still ends inside the pane.
    #[test]
    fn the_recent_menu_fits_from_the_address_field() {
        for width in [280.0, 400.0, 720.0] {
            let (left, top) = recent_menu_seat(true, width);
            let menu = aui::workbench::recent_menu_width(recent_menu_room(true, width));
            assert!(left + menu <= width - 6.0 + 0.5, "width {width}: {left} + {menu}");
            assert_eq!(top, aui::workbench::RECENT_MENU_TOP);
        }
    }
}
