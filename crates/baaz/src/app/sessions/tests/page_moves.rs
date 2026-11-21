//! Pages and sessions, both ways (docs/82 8.3): a session's Page opened in
//! Browse and brought back keeps its one page (no rebuild, no navigation);
//! Keep this tab hands a session's tab to the person, same page; Send page /
//! selection drafts the page's title, address and quoted selection; and
//! every menu offers these rows only where they work (rule 13).

use super::*;
use crate::browser::{Owner, TabId};
use crate::overlay_stack::LayerKind;

fn view_for(harness: &mut Harness, id: &str, dir: &std::path::Path, window: &mut Window, cx: &mut Context<Harness>) -> Entity<SessionView> {
    let host = crate::session::SessionHost {
        provider_id: "echo".to_owned(),
        workspace: dir.to_string_lossy().into_owned(),
        overlays: harness.overlays.clone(),
        capture: crate::runtime::CaptureToken::default(),
        terminal_host: None,
    };
    cx.new(|cx| SessionView::new(id.to_owned(), host, window, cx))
}

/// "s-1" open in Sessions with one session tab on `url`, its page live;
/// answers that tab.
fn session_with_a_page(vc: &mut gpui::VisualTestContext, baaz: &Entity<Harness>, dir: &std::path::Path, url: &str) -> TabId {
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let view = view_for(h, "s-1", dir, window, cx);
            h.active = Some(view);
            h.session_list.update(cx, |list, _| list.sessions.push(chain_muse_entry("s-1", "Fix login", 2)));
            h.set_mode(crate::layout::Mode::Sessions, window, cx);
            h.ensure_tabs_loaded(cx);
            let space = h.browser.read(cx).browse_space();
            let tab = h.browser.update(cx, |b, _| b.tabs.open(space, Owner::Session("s-1".into()), url, None));
            h.browser.update(cx, |b, cx| b.tabs_changed(cx));
            h.ensure_tab_page("s-1", tab, window, cx);
            tab
        })
    })
}

fn page_of(baaz: &Entity<Harness>, vc: &mut gpui::VisualTestContext, tab: TabId) -> (gpui::EntityId, String) {
    vc.update(|_, cx| {
        let state = baaz.read(cx).browser.read(cx).states.get(&tab).cloned().expect("the tab's page");
        let url = state.read(cx).url().to_string();
        (state.entity_id(), url)
    })
}

#[gpui::test]
fn the_page_opens_in_browse_and_comes_back_without_a_reload(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("page-two-sizes");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let tab = session_with_a_page(vc, &baaz, &state.2, "https://example.com/");
    let (page, url) = page_of(&baaz, vc, tab);
    // Expand: Browse shows that tab, its one page.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.carry_session_tab_to_browse(window, cx)));
    // (Entering Browse may build the person's own blank tab; none after.)
    let builds = vc.update(|_, cx| baaz.read(cx).browser.read(cx).browser_builds);
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse);
        assert_eq!(h.browser.read(cx).browse_centre_shown(cx), Some(tab), "Browse shows the session's tab");
        assert_eq!(h.browse_centre_state(cx).map(|s| s.entity_id()), Some(page), "the same page entity, full width");
        assert!(h.shown_page_offers(cx).collapse, "a session's page in Browse offers the way back");
    });
    // Collapse: Sessions, the session, its Page on the same tab.
    assert!(vc.update(|window, cx| baaz.update(cx, |h, cx| h.collapse_to_session_page(window, cx))));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        let layout = crate::layout::Layout::get(cx);
        assert_eq!(layout.mode, crate::layout::Mode::Sessions, "back in Sessions");
        assert_eq!(h.active_id(cx).as_deref(), Some("s-1"), "on the session");
        assert!(layout.right_open && crate::layout::right_kind(layout) == crate::layout::RightKind::Browser, "its Page open");
        assert_eq!(h.pane_tab(cx), Some(tab), "on the same tab");
        assert_eq!(h.browser.read(cx).browse_session_tab, None, "Browse holds it no more");
    });
    let (after, after_url) = page_of(&baaz, vc, tab);
    assert_eq!(after, page, "the same page entity came back: nothing reloaded");
    assert_eq!(after_url, url, "and it did not navigate");
    assert_eq!(vc.update(|_, cx| baaz.read(cx).browser.read(cx).browser_builds), builds, "no page was built on the way back");
    lane_restore(state);
}

#[gpui::test]
fn keep_this_tab_hands_the_session_tab_to_the_person_with_its_page(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("keep-tab");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let tab = session_with_a_page(vc, &baaz, &state.2, "https://example.com/");
    let (page, url) = page_of(&baaz, vc, tab);
    assert!(vc.update(|_, cx| baaz.read(cx).tab_offers(tab, cx).keep), "a session's tab offers Keep");
    // Browse shows it as the session's tab; Keep there.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.carry_session_tab_to_browse(window, cx)));
    assert!(vc.update(|_, cx| baaz.update(cx, |h, cx| h.keep_tab(tab, cx))));
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        let browser = h.browser.read(cx);
        let found = browser.tabs.get(tab).expect("the same tab");
        assert_eq!(found.owner, Owner::Person, "the person's now");
        assert_eq!(found.url, url);
        assert!(browser.tabs.tabs_of(&Owner::Session("s-1".into())).is_empty(), "the task folder no longer lists it");
        assert_eq!(browser.browse_session_tab, None, "Browse shows it as the person's");
        assert_eq!(browser.browse_centre_shown(cx), Some(tab), "still on screen");
        assert!(!h.tab_offers(tab, cx).keep, "a person's tab offers no Keep");
    });
    assert_eq!(page_of(&baaz, vc, tab), (page, url), "the same live page: nothing reloaded");
    // A person's tab cannot be kept again.
    assert!(!vc.update(|_, cx| baaz.update(cx, |h, cx| h.keep_tab(tab, cx))));
    lane_restore(state);
}

#[gpui::test]
fn send_page_and_selection_draft_the_page_into_the_composer_on_screen(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("send-page");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let tab = session_with_a_page(vc, &baaz, &state.2, "https://example.com/");
    // No selection read yet: the selection is not offered and not sent.
    assert!(!vc.update(|_, cx| baaz.read(cx).tab_offers(tab, cx).send_selection));
    assert!(!vc.update(|window, cx| baaz.update(cx, |h, cx| h.send_tab_page(tab, true, window, cx))));
    // The Page's composer is on screen: Send page drafts there, no picker.
    assert!(vc.update(|window, cx| baaz.update(cx, |h, cx| h.send_tab_page(tab, false, window, cx))));
    let (title, url) = vc.update(|_, cx| {
        let s = baaz.read(cx).browser.read(cx).states.get(&tab).cloned().expect("page");
        (s.read(cx).title().to_string(), s.read(cx).url().to_string())
    });
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert!(!h.overlays.read(cx).stack.is_open(LayerKind::AttachPicker), "the visible composer takes it");
        assert_eq!(h.active.as_ref().expect("open").read(cx).draft_text(cx), crate::browser::page_draft(&title, &url, None));
    });
    // The page answers its selection (the scripted page's fixture line).
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.browser.update(cx, |b, cx| b.ask_selection(tab, cx))));
    // The page's poll drains its answer; the reconcile it arms routes it.
    vc.executor().advance_clock(std::time::Duration::from_millis(500));
    vc.run_until_parked();
    let selection = vc.update(|_, cx| baaz.read(cx).browser.read(cx).selections.get(&tab).cloned());
    assert_eq!(selection.as_deref(), Some(aui_webview::fake::FAKE_SELECTION), "the selection landed");
    assert!(vc.update(|_, cx| baaz.read(cx).tab_offers(tab, cx).send_selection), "now offered");
    assert!(vc.update(|window, cx| baaz.update(cx, |h, cx| h.send_tab_page(tab, true, window, cx))));
    let draft = vc.update(|_, cx| baaz.read(cx).active.as_ref().expect("open").read(cx).draft_text(cx));
    assert!(draft.ends_with(&format!("> {}", aui_webview::fake::FAKE_SELECTION)), "the selection is quoted, got {draft:?}");
    lane_restore(state);
}

/// Rule 13: each menu offers a row only where it works — Send page on a
/// page (never a blank tab), Keep this tab on a session's tab only, Back to
/// the session's Page only for a project session's page in Browse (never a
/// browser chat's), Send selection only with a selection; the Field lists
/// its commands the same way, and so does the Page's nav row.
#[gpui::test]
fn the_rows_show_only_where_they_work(cx: &mut gpui::TestAppContext) {
    use crate::browser::{field_command_offered, FieldAction};
    use aui::screens::sidebar::{MenuOffers, PageAction};
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("rows-work");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let session_tab = session_with_a_page(vc, &baaz, &state.2, "https://example.com/");
    let (person_tab, blank_tab, chat_tab) = vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let space = h.browser.read(cx).browse_space();
            let person = h.browser.update(cx, |b, _| b.tabs.open(space.clone(), Owner::Person, "https://docs.rs/", None));
            let blank = h.browser.update(cx, |b, _| b.tabs.open(space.clone(), Owner::Person, crate::browser::tabs::BLANK, None));
            h.set_override("chat-1", |meta| meta.browser_chat = true, cx);
            let chat = h.browser.update(cx, |b, _| b.tabs.open(space, Owner::Session("chat-1".into()), "https://chat.example/", None));
            h.browser.update(cx, |b, cx| b.tabs_changed(cx));
            let _ = window;
            (person, blank, chat)
        })
    });
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(h.tab_offers(person_tab, cx), MenuOffers { send_page: true, ..Default::default() }, "a person's page: send only");
        assert_eq!(h.tab_offers(blank_tab, cx), MenuOffers::default(), "a blank tab offers nothing");
        assert_eq!(h.tab_offers(session_tab, cx), MenuOffers { send_page: true, keep: true, ..Default::default() }, "a session's page: send and keep");
        assert!(h.tab_offers(chat_tab, cx).keep, "a browser chat's tab can be kept too");
        // The Page pane's nav row: its tab's offers.
        let facts = h.right_facts(cx);
        assert_eq!(facts.page_offers, aui::workbench::PageNavExtras { send_page: true, keep_tab: true }, "the Page offers send and keep");
    });
    // Browse on the browser chat's tab: no way back (it has no Page).
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            h.set_mode(crate::layout::Mode::Browse, window, cx);
            h.browser.update(cx, |b, cx| b.set_session_tab(Some(chat_tab), cx));
        })
    });
    assert!(!vc.update(|_, cx| baaz.read(cx).shown_page_offers(cx).collapse), "a browser chat's page has no Page to go back to");
    // On the person's page: no keep, no way back.
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            h.browser.update(cx, |b, cx| b.set_session_tab(None, cx));
            h.browser.update(cx, |b, _| b.tabs.activate(person_tab));
            let _ = window;
        })
    });
    let offers = vc.update(|_, cx| baaz.read(cx).shown_page_offers(cx));
    assert_eq!(offers, MenuOffers { send_page: true, ..Default::default() });
    // The Field lists only what works.
    assert!(field_command_offered(FieldAction::Page(PageAction::SendPage), offers));
    for hidden in [PageAction::SendSelection, PageAction::Keep, PageAction::Collapse] {
        assert!(!field_command_offered(FieldAction::Page(hidden), offers), "{hidden:?} is not listed");
    }
    assert!(field_command_offered(FieldAction::ForgetSite, offers), "the other commands stay");
    lane_restore(state);
}
