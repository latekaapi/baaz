//! One Ask, and nothing into a composer no one can see (docs/82 8.2): ✦,
//! the Field's Ask and ⌘J open the line in place and start no session; a
//! hand-off, a screenshot or notes from Browse go to the session the person
//! picks — keyed by its id, never `active` — with a toast whose Open takes
//! them there; a cancelled pick attaches nothing; with the composer on
//! screen a screenshot attaches there, no picker.

use super::*;
use crate::app::agent_window::LineUi;
use crate::app::browser_handoff::HandoffTarget;
use crate::overlay_stack::LayerKind;

/// A 1-px PNG the composer decodes.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A session view on the scripted stand-in, not registered anywhere.
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

/// A harness with "s-1" open (titled "Fix login") and "s-2" listed
/// ("Refund page"), then Browse on example.com.
fn browse_over_a_page(vc: &mut gpui::VisualTestContext, dir: &std::path::Path) -> Entity<Harness> {
    let baaz = lane_harness(vc, dir);
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let view = view_for(h, "s-1", dir, window, cx);
            h.active = Some(view);
            h.session_list.update(cx, |list, _| {
                list.sessions.push(chain_muse_entry("s-1", "Fix login", 2));
                list.sessions.push(chain_muse_entry("s-2", "Refund page", 1));
            });
            h.set_mode(crate::layout::Mode::Browse, window, cx);
            h.ensure_browse_tab(window, cx);
            let tab = h.browser.read(cx).browse_centre_shown(cx).expect("Browse shows a tab");
            h.navigate_browse_tab(tab, "https://example.com/", window, cx);
        })
    });
    vc.run_until_parked();
    baaz
}

fn line(baaz: &Entity<Harness>, vc: &mut gpui::VisualTestContext) -> LineUi {
    vc.update(|_, cx| baaz.read(cx).browser.read(cx).agent_window.line_ui.clone())
}

fn close_line(baaz: &Entity<Harness>, vc: &mut gpui::VisualTestContext) {
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.browser.update(cx, |b, cx| b.set_agent_line(LineUi::Closed, None, cx))));
}

/// What no Ask may change: still Browse, "s-1" still the session, its
/// draft empty, nothing waiting for a session, no new row.
fn assert_nothing_drafted(baaz: &Entity<Harness>, vc: &mut gpui::VisualTestContext, how: &str) {
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "{how}: stays in Browse");
        assert_eq!(h.active_id(cx).as_deref(), Some("s-1"), "{how}: starts no session");
        assert_eq!(h.active.as_ref().expect("open").read(cx).draft_text(cx), "", "{how}: drafts nothing");
        assert!(h.browser.read(cx).parcels.is_empty(), "{how}: nothing waits for a session");
        assert_eq!(h.session_list.read(cx).sessions.len(), 2, "{how}: no new row");
    });
}

/// The newest toast: its title and its action's id.
fn newest_toast(baaz: &Entity<Harness>, vc: &mut gpui::VisualTestContext) -> (String, Option<String>) {
    vc.update(|_, cx| {
        let toast = baaz.read(cx).overlays.read(cx).toasts.last().cloned().expect("a toast");
        (toast.title.to_string(), toast.actions.first().map(|a| a.id.to_string()))
    })
}

#[gpui::test]
fn star_the_fields_ask_and_cmd_j_open_the_line_and_start_no_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask");
    let vc = cx.add_empty_window();
    let baaz = browse_over_a_page(vc, &state.2);
    // ✦: the line, ready to type, "This page" standing.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.browse_ask_page(window, cx)));
    assert_eq!(line(&baaz, vc), LineUi::Ask, "✦ opens the line");
    assert!(vc.update(|_, cx| baaz.read(cx).browser.read(cx).agent_window.line_page_chip), "on the page shown");
    assert_nothing_drafted(&baaz, vc, "✦");
    close_line(&baaz, vc);
    // ⌘J: the same line.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_agent_line(window, cx)));
    assert_eq!(line(&baaz, vc), LineUi::Ask, "⌘J opens the line");
    assert_nothing_drafted(&baaz, vc, "⌘J");
    close_line(&baaz, vc);
    // The Field's Ask: the line, with the question asked there in place
    // (or, with no provider to ask, the line saying so) — never a session.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.browse_ask("what is the refund window?", window, cx)));
    vc.run_until_parked();
    let asked = line(&baaz, vc);
    let notice = vc.update(|_, cx| baaz.read(cx).browser.read(cx).agent_window.line_notice.clone());
    assert!(
        matches!(&asked, LineUi::Answer { question, .. } if question == "what is the refund window?") || (asked == LineUi::Ask && notice.is_some()),
        "the Field's Ask asks on the line, got {asked:?} / {notice:?}"
    );
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "the Field's Ask stays in Browse");
        assert_eq!(h.active_id(cx).as_deref(), Some("s-1"), "the Field's Ask opens no session");
        assert_eq!(h.active.as_ref().expect("open").read(cx).draft_text(cx), "", "the Field's Ask drafts nothing");
        assert!(h.browser.read(cx).parcels.is_empty(), "nothing waits for a session");
    });
    lane_restore(state);
}

#[gpui::test]
fn the_fields_ask_on_a_new_tab_asks_with_no_page(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-blank");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let view = view_for(h, "s-1", &state.2, window, cx);
            h.active = Some(view);
            h.set_mode(crate::layout::Mode::Browse, window, cx);
            h.ensure_browse_tab(window, cx);
        })
    });
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.browse_ask("hello", window, cx)));
    assert_ne!(line(&baaz, vc), LineUi::Closed, "the line opens on a new tab too");
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert!(!h.browser.read(cx).agent_window.line_page_chip, "a new tab is no page to ask about");
        assert_eq!(h.active_id(cx).as_deref(), Some("s-1"), "no session");
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse);
    });
    lane_restore(state);
}

#[gpui::test]
fn a_hand_off_stays_on_the_page_and_its_toasts_open_takes_you_there(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-handoff");
    let vc = cx.add_empty_window();
    let baaz = browse_over_a_page(vc, &state.2);
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let chat = view_for(h, "chat-1", &state.2, window, cx);
            h.set_browser_chat(chat, cx);
        })
    });
    // "s-2" has no view yet: the draft waits for it, keyed by its id.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.hand_off_browser_chat(HandoffTarget::Session("s-2".into()), window, cx)));
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "the hand-off leaves the person on the page");
        assert_eq!(h.active.as_ref().expect("open").read(cx).draft_text(cx), "", "the open session gets nothing");
        let waiting: Vec<_> = h.browser.read(cx).parcels.iter().map(|p| p.target.clone()).collect();
        assert_eq!(waiting, vec![Some("s-2".to_owned())], "the draft waits for its own session");
    });
    let (title, action) = newest_toast(&baaz, vc);
    assert_eq!(title, "Continued in Refund page");
    let target = action.as_deref().and_then(crate::overlays::toast_open_target).map(str::to_owned);
    assert_eq!(target.as_deref(), Some("s-2"), "the toast's action is Open on the target");
    // Open: that session, in Sessions, its draft landed there.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.open_parcel_target(target.clone().expect("open"), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Sessions, "Open switches to Sessions");
        assert_eq!(h.active_id(cx).as_deref(), Some("s-2"), "Open opens the target");
        let draft = h.active.as_ref().expect("open").read(cx).draft_text(cx);
        assert!(draft.starts_with("From my browser chat"), "the hand-off draft landed in its target, got {draft:?}");
        assert!(h.browser.read(cx).parcels.is_empty(), "taken on landing");
    });
    lane_restore(state);
}

#[gpui::test]
fn a_browse_screenshot_asks_which_session_and_goes_to_the_pick_only(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-shot");
    let vc = cx.add_empty_window();
    let baaz = browse_over_a_page(vc, &state.2);
    // "s-2" is parked: loaded, not on screen.
    let parked = vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let view = view_for(h, "s-2", &state.2, window, cx);
            h.registry.park("s-2".to_owned(), view.clone(), cx);
            view
        })
    });
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.attach_browser_screenshot(PNG.to_vec(), window, cx)));
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert!(h.overlays.read(cx).stack.is_open(LayerKind::AttachPicker), "Browse shows no composer: the picker asks");
        assert!(h.active.as_ref().expect("open").read(cx).draft_content_empty(cx), "nothing lands before a pick");
    });
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.attach_picked(HandoffTarget::Session("s-2".into()), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert!(!h.overlays.read(cx).stack.is_open(LayerKind::AttachPicker), "the pick closes the picker");
        assert!(!parked.read(cx).draft_content_empty(cx), "the picked session has the screenshot");
        assert!(h.active.as_ref().expect("open").read(cx).draft_content_empty(cx), "the active session is untouched");
        assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "the person stays on the page");
    });
    let (title, action) = newest_toast(&baaz, vc);
    assert_eq!(title, "Screenshot added to Refund page");
    assert_eq!(action.as_deref().and_then(crate::overlays::toast_open_target), Some("s-2"));
    lane_restore(state);
}

#[gpui::test]
fn a_cancelled_pick_attaches_nothing_anywhere(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-cancel");
    let vc = cx.add_empty_window();
    let baaz = browse_over_a_page(vc, &state.2);
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.attach_browser_screenshot(PNG.to_vec(), window, cx)));
    let toasts = vc.update(|_, cx| baaz.read(cx).overlays.read(cx).toasts.len());
    // Escape (or a click on the scrim): the stack drops the entry.
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.pop_overlay_entry(cx)));
    vc.run_until_parked();
    // A pick after the cancel finds no parcel.
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.attach_picked(HandoffTarget::Session("s-1".into()), window, cx)));
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert!(!h.overlays.read(cx).stack.is_open(LayerKind::AttachPicker), "the picker closed");
        assert!(h.active.as_ref().expect("open").read(cx).draft_content_empty(cx), "nothing attached");
        assert!(h.browser.read(cx).parcels.is_empty(), "nothing waits");
        assert_eq!(h.overlays.read(cx).toasts.len(), toasts, "no toast");
    });
    lane_restore(state);
}

#[gpui::test]
fn with_the_composer_on_screen_a_screenshot_attaches_without_a_picker(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-visible");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|window, cx| {
        baaz.update(cx, |h, cx| {
            let view = view_for(h, "s-1", &state.2, window, cx);
            h.active = Some(view);
            h.set_mode(crate::layout::Mode::Sessions, window, cx);
        })
    });
    assert!(vc.update(|_, cx| baaz.read(cx).composer_on_screen(cx)), "Sessions with a session open shows its composer");
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.attach_browser_screenshot(PNG.to_vec(), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert!(!h.overlays.read(cx).stack.is_open(LayerKind::AttachPicker), "no picker");
        assert!(!h.active.as_ref().expect("open").read(cx).draft_content_empty(cx), "the visible composer has it");
    });
    lane_restore(state);
}

/// A new session's parcel (a pick of "New session in <project>") lands in
/// the session that open makes — never whatever is open — and a `new`
/// refused by a switch already in flight drops it.
#[gpui::test]
fn a_new_sessions_parcel_is_keyed_to_its_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-new");
    let vc = cx.add_empty_window();
    let baaz = browse_over_a_page(vc, &state.2);
    let unrelated = vc.update(|_, cx| baaz.read(cx).active.clone().expect("open"));
    let project = vc.update(|_, cx| {
        baaz.update(cx, |h, cx| {
            let id = h.session_list.update(cx, |list, _| list.projects.add(&state.2).id.clone());
            h.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
            h.current_project = Some(id.clone());
            id
        })
    });
    let went = vc.update(|window, cx| {
        baaz.update(cx, |h, cx| h.deliver_parcel(HandoffTarget::Project(project.clone()), crate::browser::Parcel::Text("about this page".into()), window, cx))
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        let active = h.active.clone().expect("the new session is open");
        let id = active.read(cx).rt(cx).session_id.clone();
        assert_ne!(id, "s-1", "a new session");
        assert_eq!(went.as_deref(), Some(id.as_str()), "the parcel says where it went");
        assert_eq!(active.read(cx).draft_text(cx), "about this page");
        assert!(h.browser.read(cx).parcels.is_empty(), "taken on landing");
        assert_eq!(unrelated.read(cx).draft_text(cx), "", "the unrelated session keeps no draft");
    });
    let (title, action) = newest_toast(&baaz, vc);
    assert!(title.starts_with("Draft") || title.starts_with("Sent to a new session in"), "a landing toast, got {title:?}");
    assert!(action.as_deref().and_then(crate::overlays::toast_open_target).is_some(), "with Open");
    // A `new` refused by a switch already in flight drops it.
    vc.update(|_, cx| baaz.update(cx, |h, _| h.open_state.switch_pending = true));
    let went = vc.update(|window, cx| {
        baaz.update(cx, |h, cx| h.deliver_parcel(HandoffTarget::Project(project.clone()), crate::browser::Parcel::Text("a second".into()), window, cx))
    });
    vc.update(|_, cx| {
        let h = baaz.read(cx);
        assert_eq!(went, None, "it went nowhere");
        assert!(h.browser.read(cx).parcels.is_empty(), "the refused open leaves nothing waiting");
        assert_eq!(h.active.clone().expect("open").read(cx).draft_text(cx), "about this page", "appended nowhere");
    });
    lane_restore(state);
}

/// A parcel waits for its keyed session: an unrelated landing takes
/// nothing; only its own session's landing takes it, once.
#[gpui::test]
fn a_parcel_waits_for_its_keyed_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("one-ask-waits");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (view_a, view_b) = vc.update(|window, cx| {
        baaz.update(cx, |h, cx| (view_for(h, "s-1", &state.2, window, cx), view_for(h, "s-2", &state.2, window, cx)))
    });
    vc.update(|_, cx| {
        baaz.update(cx, |h, cx| {
            h.browser.update(cx, |browser, _| {
                browser.parcels.push(crate::browser::PendingParcel { target: Some("s-2".into()), parcel: crate::browser::Parcel::Text("keyed".into()), announce: None })
            });
        })
    });
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.activate(view_a.clone(), false, window, cx)));
    vc.update(|_, cx| {
        assert_eq!(view_a.read(cx).draft_text(cx), "", "the unrelated landing takes nothing");
        assert_eq!(baaz.read(cx).browser.read(cx).parcels.len(), 1, "it still waits");
    });
    vc.update(|window, cx| baaz.update(cx, |h, cx| h.activate(view_b.clone(), false, window, cx)));
    vc.update(|_, cx| {
        assert_eq!(view_b.read(cx).draft_text(cx), "keyed");
        assert!(baaz.read(cx).browser.read(cx).parcels.is_empty(), "taken once");
        assert_eq!(view_a.read(cx).draft_text(cx), "");
    });
    lane_restore(state);
}
