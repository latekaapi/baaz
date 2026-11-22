//! R4 Peek and Split (docs/27 §5; the agentic-ui gallery's approved Peek and
//! Split, wired here; owner 11 Nov: ⇧-click peeks, ⌥-click splits).
//!
//! **Peek** is a loose page — no tab — over the Browse centre: 72 % × 82 %,
//! rounded, with the gallery's 35 % scrim. Nothing draws over a live
//! Chromium page, so while a Peek stands open the page beneath freezes into
//! its screenshot stand-in (dimmed, a click on it closes Peek) and the ×
//! is cut through the Peek page as a hole. The host caption sits under the
//! card. Esc closes; ⌘↩ (or "Open as tab") makes it a real tab with the
//! same page, never reloaded.
//!
//! **Split** is two of your tabs side by side in the centre. The one shown
//! (the sidebar's address line) is the focused half; a click into the other
//! half focuses it. Each half has a × (a hole in its page): it leaves Split
//! and keeps the tab (owner 11 Nov).

use std::sync::atomic::{AtomicBool, Ordering};

use aui_webview::{FakeWebBackend, LinkGesture, WebBackend as _, WebviewState};
use gpui::{prelude::*, Bounds, Context, Entity, Pixels, Window};

use super::tabs::{Owner, TabId};
use crate::app::Harness;
use super::BrowserView;
use crate::layout::LayoutGlobal as _;

/// Whether a Peek stands open: the CEF key router reads it from its
/// callback (no Harness there) to claim Esc and ⌘↩ for the Peek.
pub(crate) static PEEK_OPEN: AtomicBool = AtomicBool::new(false);

/// The keystrokes the router claims while a Peek is open.
#[cfg_attr(not(feature = "cef"), allow(dead_code))]
pub(crate) fn peek_claims(keystroke: &str) -> bool {
    PEEK_OPEN.load(Ordering::Relaxed) && matches!(keystroke, "escape" | "cmd-enter")
}

/// The Peek card's share of the centre (the gallery's 72 % × 82 %).
pub(crate) const PEEK_W: f32 = 0.72;
pub(crate) const PEEK_H: f32 = 0.82;
/// The × hole: 24 px, 8 in from the card's top-right corner.
pub(crate) const CLOSE: f32 = 24.0;
pub(crate) const CLOSE_INSET: f32 = 8.0;
/// The gap between the two halves of a Split.
pub(crate) const SPLIT_GAP: f32 = 6.0;

/// An open Peek.
pub(crate) struct Peek {
    pub url: String,
    pub state: Entity<WebviewState>,
    /// The tab whose link was ⇧-clicked: "Open as tab" lands after it.
    pub opener: TabId,
}

/// Two tabs side by side; `left` was the shown tab when it opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SplitPair {
    pub left: TabId,
    pub right: TabId,
}

impl SplitPair {
    /// The other half of `id`, when `id` is a half.
    pub(crate) fn partner(&self, id: TabId) -> Option<TabId> {
        if id == self.left {
            Some(self.right)
        } else if id == self.right {
            Some(self.left)
        } else {
            None
        }
    }
}

/// The Peek card's rect inside the centre rect `centre`.
pub(crate) fn peek_rect(centre: Bounds<Pixels>) -> Bounds<Pixels> {
    let w = centre.size.width * PEEK_W;
    let h = centre.size.height * PEEK_H;
    let origin = centre.origin + gpui::point((centre.size.width - w) / 2.0, (centre.size.height - h) / 2.0);
    Bounds::new(origin, gpui::size(w, h))
}

/// The × rect at the top-right of `card`.
pub(crate) fn close_rect(card: Bounds<Pixels>) -> Bounds<Pixels> {
    let x = card.origin.x + card.size.width - gpui::px(CLOSE + CLOSE_INSET);
    let y = card.origin.y + gpui::px(CLOSE_INSET);
    Bounds::new(gpui::point(x, y), gpui::size(gpui::px(CLOSE), gpui::px(CLOSE)))
}

/// How narrow either half of a Split may be dragged: a fifth of the centre.
pub(crate) const SPLIT_MIN: f32 = 0.2;

/// `ratio` held to [`SPLIT_MIN`]..=1 − [`SPLIT_MIN`].
pub(crate) fn clamp_split(ratio: f32) -> f32 {
    if ratio.is_finite() { ratio.clamp(SPLIT_MIN, 1.0 - SPLIT_MIN) } else { 0.5 }
}

/// The left half's width for `ratio` of the centre (gap excluded).
pub(crate) fn split_left_width(centre: Bounds<Pixels>, ratio: f32) -> Pixels {
    ((centre.size.width - gpui::px(SPLIT_GAP)) * clamp_split(ratio)).round()
}

/// The ratio that puts the divider's middle under window x `x`.
/// One step of the Split divider's drag ([`BrowserView::split_drag`]).
pub(crate) enum SplitDrag {
    /// The press on the divider.
    Press,
    /// A move: the left half's new share of the centre.
    Move(f32),
    /// The release.
    Release,
}

pub(crate) fn split_ratio_at(centre: Bounds<Pixels>, x: f32) -> f32 {
    let usable = f32::from(centre.size.width) - SPLIT_GAP;
    if usable <= 0.0 {
        return 0.5;
    }
    clamp_split((x - f32::from(centre.origin.x) - SPLIT_GAP / 2.0) / usable)
}

/// The two halves of a Split inside `centre`, the left one `ratio` of it
/// (owner 12 Nov: the halves resize by the divider between them).
pub(crate) fn split_rects(centre: Bounds<Pixels>, ratio: f32) -> (Bounds<Pixels>, Bounds<Pixels>) {
    let gap = gpui::px(SPLIT_GAP);
    let left_w = split_left_width(centre, ratio);
    let right_w = centre.size.width - gap - left_w;
    let left = Bounds::new(centre.origin, gpui::size(left_w, centre.size.height));
    let right = Bounds::new(centre.origin + gpui::point(left_w + gap, gpui::px(0.0)), gpui::size(right_w, centre.size.height));
    (left, right)
}

impl Harness {
    /// A page that belongs to no tab (Peek): the engine the tabs use.
    fn loose_page(&mut self, url: &str, opener: TabId, window: &Window, cx: &mut Context<Self>) -> Entity<WebviewState> {
        let fake = self.browser.read(cx).fake || cfg!(all(test, feature = "probe"));
        let _ = window;
        #[cfg_attr(not(feature = "cef"), allow(unused_variables))]
        let profile = self.browser.read(cx).tab_profile(opener);
        cx.new(|cx| {
            if !fake {
                #[cfg(feature = "cef")]
                if let Some(backend) = super::cef_backend(window, url, (255, 255, 255, 255), profile.as_deref()) {
                    return WebviewState::new(backend, cx);
                }
            }
            let mut backend = FakeWebBackend::new();
            backend.navigate(url);
            WebviewState::new(Box::new(backend), cx)
        })
    }

    /// The pages' ⇧- and ⌥-clicked links (R4). In Browse a person tab's
    /// link peeks or splits; anywhere else (a session's pane) the link opens
    /// as a background tab, as before.
    pub(crate) fn drain_link_gestures(&mut self, window: &Window, cx: &mut Context<Self>) {
        let openers: Vec<(TabId, Entity<WebviewState>)> =
            self.browser.read(cx).states.iter().map(|(id, state)| (*id, state.clone())).collect();
        let peek_page = self.browser.read(cx).peek.as_ref().map(|peek| (peek.opener, peek.state.clone()));
        let mut gestures: Vec<(TabId, String, LinkGesture)> = Vec::new();
        for (id, state) in openers {
            for (url, gesture) in state.update(cx, |state, _| state.take_link_gestures()) {
                gestures.push((id, url, gesture));
            }
        }
        // A gesture inside the Peek itself navigates the Peek.
        if let Some((opener, state)) = peek_page {
            for (url, _) in state.update(cx, |state, _| state.take_link_gestures()) {
                gestures.push((opener, url, LinkGesture::Peek));
            }
        }
        for (opener, url, gesture) in gestures {
            let person = matches!(self.browser.read(cx).tabs.get(opener).map(|tab| &tab.owner), Some(Owner::Person));
            let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
            crate::baaz_log!("browse: {gesture:?} {url} from tab {}", opener.0);
            match gesture {
                LinkGesture::Peek if person && in_browse => self.open_peek(opener, &url, window, cx),
                LinkGesture::Split if person && in_browse => self.open_split(opener, &url, cx),
                _ => {
                    if self.browser.update(cx, |browser, _| super::chrome::place_popup_tab(&mut browser.tabs, opener, &url, false)).is_some() {
                        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                    }
                }
            }
            cx.notify();
        }
    }

    /// Peek `url` over the centre (a second ⇧-click navigates the open Peek).
    pub(crate) fn open_peek(&mut self, opener: TabId, url: &str, window: &Window, cx: &mut Context<Self>) {
        if let Some(peek) = self.browser.update(cx, |browser, _| {
            browser.peek.as_mut().map(|peek| {
                peek.url = url.to_owned();
                peek.state.clone()
            })
        }) {
            peek.update(cx, |state, _| state.navigate(url));
            cx.notify();
            return;
        }
        let state = self.loose_page(url, opener, window, cx);
        state.update(cx, |state, _| state.set_corner_radius(12.0));
        self.browser.update(cx, |browser, cx| browser.adopt_peek(Peek { url: url.to_owned(), state, opener }, cx));
        PEEK_OPEN.store(true, Ordering::Relaxed);
        // A fresh Peek takes the keyboard (so Esc and ⌘↩ reach it), once its
        // page is laid out: after the frame that mounts it.
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| {
                if let Some(peek) = this.browser.read(cx).peek.as_ref() {
                    let state = peek.state.clone();
                    let focus = state.read(cx).focus_handle().clone();
                    window.focus(&focus, cx);
                    state.update(cx, |state, _| state.focus_page());
                }
            });
        });
        cx.notify();
    }


    /// ⌘↩: the Peek becomes a real tab after its opener, with the same page.
    pub(crate) fn peek_to_tab(&mut self, cx: &mut Context<Self>) {
        let Some(peek) = self.browser.update(cx, |browser, _| {
            browser.peek_watch = None;
            browser.peek.take()
        }) else {
            return;
        };
        PEEK_OPEN.store(false, Ordering::Relaxed);
        let url = peek.state.read(cx).url().to_string();
        let url = if url.is_empty() { peek.url.clone() } else { url };
        if let Some(id) = self.browser.update(cx, |browser, _| super::chrome::place_popup_tab(&mut browser.tabs, peek.opener, &url, true)) {
            peek.state.update(cx, |state, _| state.set_corner_radius(0.0));
            self.browser.update(cx, |browser, cx| browser.adopt_page(id, peek.state, cx));
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        cx.notify();
    }

    /// ⌥-click: `url` opens as a tab beside `opener`, side by side with it.
    pub(crate) fn open_split(&mut self, opener: TabId, url: &str, cx: &mut Context<Self>) {
        let Some(right) = self.browser.update(cx, |browser, _| super::chrome::place_popup_tab(&mut browser.tabs, opener, url, false)) else { return };
        self.browser.update(cx, |browser, _| browser.split = Some(SplitPair { left: opener, right }));
        self.browser.update(cx, |browser, _| browser.split_ratio = 0.5);
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
    }

    /// A half's ×: leave Split, keep both tabs. Closing the shown half
    /// shows the other.
    pub(crate) fn close_split_half(&mut self, half: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pair) = self.browser.update(cx, |browser, _| browser.split.take()) else { return };
        // Either half may hold the keyboard; the one that leaves the centre
        // hides with it, and keys sent to a hidden page go nowhere (found
        // live 11 Nov). Hand it back to the window first.
        for id in [pair.left, pair.right] {
            if let Some(state) = self.browser.read(cx).states.get(&id).cloned() {
                state.update(cx, |state, _| state.release_keyboard());
            }
        }
        window.focus(&self.focus_root, cx);
        if self.browser.read(cx).browse_centre_shown(cx) == Some(half) {
            if let Some(other) = pair.partner(half) {
                self.select_browse_sidebar_tab(other, window, cx);
            }
        }
        cx.notify();
    }


    #[cfg(feature = "probe")]
    /// `peek:<url>` / `split:<url>`: the ⇧/⌥-click on the shown tab's link,
    /// without a page to click (captures). A URL that is not a web page,
    /// or no tab shown in the Browse centre, is an `Err`. Free.
    pub(crate) fn step_link_gesture(&mut self, rest: &str, gesture: LinkGesture, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.ensure_tabs_loaded(cx);
        let url = rest.trim();
        if !super::history::recordable(url) {
            return Err(format!("`{url}` is not a web page to open"));
        }
        let opener = self.browser.read(cx).browse_centre_shown(cx).ok_or("no tab is shown in Browse")?;
        match gesture {
            LinkGesture::Peek => self.open_peek(opener, url, window, cx),
            LinkGesture::Split => {
                // `open_split` gives up without a word when the tab model
                // refuses the new half; a fresh half is a new split pair.
                let before = self.browser.read(cx).split.map(|pair| (pair.left, pair.right));
                self.open_split(opener, url, cx);
                let after = self.browser.read(cx).split.map(|pair| (pair.left, pair.right));
                if after.is_none() || after == before {
                    return Err("the Split's second half could not be placed".into());
                }
            }
        }
        Ok(())
    }
}

impl BrowserView {
    /// The Split partner shown beside the centre tab, when the centre tab is
    /// a half of the open Split and both tabs still exist.
    pub(crate) fn split_partner(&self, cx: &gpui::App) -> Option<TabId> {
        let pair = self.split?;
        let shown = self.browse_centre_shown(cx)?;
        let other = pair.partner(shown)?;
        self.tabs.get(other).is_some().then_some(other)
    }
    /// One step of the Split divider's drag, redrawn when it moved
    /// something: the press arms it (the pages stop taking the pointer),
    /// a move sets the left half's share, the release disarms it.
    pub(crate) fn split_drag(&mut self, step: SplitDrag, cx: &mut Context<Self>) {
        let moved = match step {
            SplitDrag::Press => !std::mem::replace(&mut self.split_dragging, true),
            SplitDrag::Move(ratio) => std::mem::replace(&mut self.split_ratio, ratio) != ratio,
            SplitDrag::Release => std::mem::replace(&mut self.split_dragging, false),
        };
        if moved {
            cx.notify();
        }
    }

    /// Close the Peek (Esc, ×, a click on the page beneath).
    pub(crate) fn close_peek(&mut self, cx: &mut Context<Self>) {
        self.peek_watch = None;
        if let Some(peek) = self.peek.take() {
            // Its page may hold the keyboard: hand it back before it goes.
            peek.state.update(cx, |state, _| state.release_keyboard());
            PEEK_OPEN.store(false, Ordering::Relaxed);
            cx.notify();
        }
    }



    /// Whether the open Split is still valid; a closed half ends it.
    pub(crate) fn prune_split(&mut self) {
        if let Some(pair) = self.split {
            if self.tabs.get(pair.left).is_none() || self.tabs.get(pair.right).is_none() {
                self.split = None;
            }
        }
    }
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(gpui::point(gpui::px(x), gpui::px(y)), gpui::size(gpui::px(w), gpui::px(h)))
    }

    #[test]
    fn the_peek_card_is_centred_at_the_gallery_share() {
        let card = peek_rect(rect(100.0, 0.0, 1000.0, 800.0));
        assert_eq!(card, rect(240.0, 72.0, 720.0, 656.0));
        assert_eq!(close_rect(card), rect(928.0, 80.0, 24.0, 24.0));
    }

    #[test]
    fn split_halves_share_the_centre_with_a_gap() {
        let (l, r) = split_rects(rect(0.0, 0.0, 1006.0, 800.0), 0.5);
        assert_eq!(l, rect(0.0, 0.0, 500.0, 800.0));
        assert_eq!(r, rect(506.0, 0.0, 500.0, 800.0));
        // Dragged: a 30 / 70 split, and never past a fifth either way.
        let (l, r) = split_rects(rect(0.0, 0.0, 1006.0, 800.0), 0.3);
        assert_eq!((l, r), (rect(0.0, 0.0, 300.0, 800.0), rect(306.0, 0.0, 700.0, 800.0)));
        let (l, _) = split_rects(rect(0.0, 0.0, 1006.0, 800.0), 0.05);
        assert_eq!(l.size.width, gpui::px(200.0));
        // The divider follows the pointer: its middle lands under x.
        let centre = rect(100.0, 0.0, 1006.0, 800.0);
        assert!((split_ratio_at(centre, 100.0 + 300.0 + 3.0) - 0.3).abs() < 1e-4);
        assert_eq!(split_ratio_at(centre, 0.0), SPLIT_MIN);
        assert_eq!(split_ratio_at(centre, 5000.0), 1.0 - SPLIT_MIN);
    }

    #[test]
    fn a_split_names_each_half_s_partner() {
        let pair = SplitPair { left: TabId(1), right: TabId(2) };
        assert_eq!(pair.partner(TabId(1)), Some(TabId(2)));
        assert_eq!(pair.partner(TabId(2)), Some(TabId(1)));
        assert_eq!(pair.partner(TabId(3)), None);
    }

    #[test]
    fn the_router_claims_esc_and_cmd_enter_only_while_peeking() {
        PEEK_OPEN.store(false, Ordering::Relaxed);
        assert!(!peek_claims("escape"));
        PEEK_OPEN.store(true, Ordering::Relaxed);
        assert!(peek_claims("escape") && peek_claims("cmd-enter"));
        assert!(!peek_claims("cmd-l"));
        PEEK_OPEN.store(false, Ordering::Relaxed);
    }
}
