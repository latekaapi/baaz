//! One owner for what covers the native pages (task 4.5b, plan row 4.5:
//! "one `NativeOcclusion` owner for cover/park/holes").
//!
//! The Chromium (or WKWebView) page is a child window drawn ABOVE everything
//! gpui paints, so gpui cannot draw over it: whatever covers a page makes the
//! host hide it (the page then paints its fresh still picture, `set_obscured`)
//! or cut a hole through it where a small piece of chrome must show. Every
//! Browse rendering fault so far traced back to some caller deciding this on
//! its own. Here it is decided once per frame, in one place:
//!
//! * the root reports what covers the pages — the regions' `covers_page`
//!   answers and the overlay state it owns — as one [`Covers`]
//!   (`Harness::page_covers`);
//! * [`BrowserView::occlude`] answers which page shows ([`PageShow`]),
//!   stores the centre page's rect, and sets every live page's obscured flag
//!   from that answer ([`BrowserView::apply_page_visibility`]);
//! * [`BrowserView::cut_holes`] cuts the shell's chrome over the centre (the
//!   sidebar peek), the Peek's and Split's ×, and the loading line through
//!   the shown page, and takes them back from a page that stopped being the
//!   centre;
//! * [`NativeOcclusion::settling`] says whether a just-covered page still
//!   waits for its picture, so the root holds its covering UI back.
//!
//! [`browser_visible`] stays the one pure predicate behind every answer.
//! The overlay stack (plan 4.6) plugs in at [`Covers`]: its entries'
//! `covers_page` is the one overlay input ([`BrowserVisibility::overlay_open`]).

use std::cell::Cell;
use std::rc::Rc;

use aui_webview::WebviewState;
use gpui::{Bounds, Context, Entity, Pixels, WeakEntity};

use super::peek_split::{self, CLOSE, CLOSE_INSET};
use crate::overlay_stack::LayerKind;
use super::{chrome, tabs, BrowserEvent, BrowserView, TabId, BLANK};
use crate::layout::LayoutGlobal as _;

/// Everything that decides whether one session's native view may show.
///
/// All fields are positive facts about the frame; [`browser_visible`] ANDs
/// the good ones and NANDs the covering ones. One struct so the call site in
/// `render` names every input and the truth table below covers every input.
/// Every cover is something gpui draws in the main window, under the page:
/// what an overlay window draws (Browse's modals, right-click menus, the
/// Field, toasts) sits above the live page and is no input here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BrowserVisibility {
    /// The right pane stands open.
    pub pane_open: bool,
    /// The pane shows the Browser kind.
    pub kind_browser: bool,
    /// The frame shows Browse mode's centre page instead of the pane's:
    /// the current project's person-tab page, which needs no open pane
    /// and no Browser kind. Sessions mode always leaves this false, so
    /// its visibility reads exactly as before.
    pub centre_browse: bool,
    /// This webview's session is the active one.
    pub session_active: bool,
    /// The Settings page stands open.
    pub settings_open: bool,
    /// An entry on the overlay stack covers the page: one that draws in the
    /// main window and says it covers ([`crate::overlay_stack::OverlayStack::covers_page`]) —
    /// the dialog and palette in Sessions (Browse hosts them in an overlay
    /// window), the menus, the composer's `+` menu, the sign-in sheets.
    pub overlay_open: bool,
    /// The file-drop overlay covers the session's column. The pane's only:
    /// Browse mounts no drop target.
    pub drop_cover: bool,
    /// The Browse omnibox suggestion list stands open in the main window:
    /// the Field's rows draw there when it has no overlay window (the
    /// sidebar closed). The page shows its screenshot stand-in under them.
    pub omnibox_open: bool,
    /// A Browse chrome popover (downloads, extensions) stands open. They
    /// draw in the main window, over the page, so the page shows its
    /// stand-in, like the omnibox above.
    pub chrome_popover_open: bool,
    /// The Baaz-drawn crashed page covers the page rect. The library
    /// reports no load-error event — only `Crashed` — so the error page
    /// renders from that state alone, and the native view hides under it.
    pub page_error_open: bool,
}

/// Whether `inputs`' native view may show: the pane open on the Browser
/// kind for the active session — or, in Browse, the centre page — with
/// no overlay over it and a live window. The covering rules are the
/// same for both: the palette, menus, dialogs and settings obscure the
/// centre page exactly as they obscure the pane's.
pub(crate) fn browser_visible(inputs: BrowserVisibility) -> bool {
    (inputs.pane_open && inputs.kind_browser || inputs.centre_browse)
        && inputs.session_active
        && !inputs.settings_open
        && !inputs.overlay_open
        && !inputs.drop_cover
        && !inputs.omnibox_open
        && !inputs.chrome_popover_open
        && !inputs.page_error_open
}

/// `base` as the Browse centre page reads it: its tab and liveness, its own
/// chrome popovers, and not the file-drop overlay, which Browse never draws:
/// the flag can stand stale from Sessions (a drag that left the window), and
/// a stale cover would freeze the page under nothing. (The composer's `+`
/// menu cannot stand stale: a mode switch closes the menus on the overlay
/// stack, `Harness::set_mode`.)
pub(super) fn centre_visibility(base: BrowserVisibility, shown: bool, live: bool, chrome_popover_open: bool) -> BrowserVisibility {
    BrowserVisibility {
        centre_browse: shown,
        session_active: live,
        chrome_popover_open,
        drop_cover: false,
        ..base
    }
}

/// What covers the native pages this frame, as only the root knows it:
/// the overlays it owns and the other regions' `covers_page` answers
/// (`Harness::page_covers`). The view adds what it draws itself — the
/// file-drop cover, the crashed page, its chrome popovers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Covers {
    /// The overlay and region inputs of the pure predicate. `drop_cover`,
    /// `page_error_open`, `chrome_popover_open`, `centre_browse` and
    /// `session_active` are the view's own and are filled in by
    /// [`BrowserView::occlude`].
    pub base: BrowserVisibility,
    /// The right pane covers its Page (its strip slides, or it draws Baaz's
    /// own state there): the region contract's `covers_page`.
    pub pane_covered: bool,
    /// The frame owner's active tab when its owner is the frame's owner —
    /// the pane's page — or `None` (no page, or a provider switch pending).
    pub pane_tab: Option<TabId>,
    /// The Browse centre may show its page at all: no pending switch and
    /// no full-page region (Skills) over it.
    pub centre_allowed: bool,
    /// The centre page's column rect for this window, before the in-flow
    /// strips (find, permission) take their rows above the page.
    pub centre_rect: Bounds<Pixels>,
}

/// The frame's answer: which pages show live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PageShow {
    /// The pane's page (Sessions mode), when the frame shows one.
    pub pane: Option<TabId>,
    /// Whether the pane's page is live (nothing over it).
    pub pane_live: bool,
    /// The Browse centre's tab, when Browse shows one.
    pub centre: Option<TabId>,
    /// Whether the centre's page is live (nothing over it).
    pub centre_live: bool,
    /// The view itself draws over the centre page (a chrome popover, the
    /// crashed page): its region's `covers_page`.
    pub centre_covered_by_view: bool,
}

impl PageShow {
    /// The tab on screen: the centre's in Browse, else the pane's. What the
    /// done-task sweep and the discard spare.
    pub(crate) fn on_screen(&self) -> Option<TabId> {
        self.centre.or(self.pane)
    }
}

/// The one owner of cover, park and holes (see the module doc).
#[derive(Default)]
pub(crate) struct NativeOcclusion {
    /// The page the holes were last cut through, so they are taken back
    /// when it stops being the centre.
    holes_on: Option<WeakEntity<WebviewState>>,
    /// The loading line's rect while a page loads (v3): cut through the
    /// page as a hole so the line shows along the page column's top edge.
    /// Cleared by the view's render, set by the line's prepaint.
    pub(crate) progress_hole: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The last frame's answer.
    shown: PageShow,
}

impl NativeOcclusion {
    /// The last frame's answer.
    pub(crate) fn shown(&self) -> PageShow {
        self.shown
    }

    /// Whether a page that was just covered is still live, waiting for the
    /// picture that stands in for it ([`WebviewState::cover_settling`]).
    /// The frame holds its covering UI back until it is not: gpui cannot
    /// paint over the live page, so a cover drawn sooner showed only its
    /// part over the gpui ground — the ⌘K scrim dimmed the sidebar ~165 ms
    /// before the page (owner recording 2026-10-11).
    pub(crate) fn settling<'a>(pages: impl IntoIterator<Item = &'a Entity<WebviewState>>, cx: &gpui::App) -> bool {
        pages.into_iter().any(|state| state.read(cx).cover_settling())
    }

    /// Hand `holes` to `current` (the centre's page), taking them back from
    /// the page they were last cut through when that is another one.
    fn hand_holes(&mut self, holes: Vec<[f32; 5]>, current: Option<Entity<WebviewState>>, cx: &mut gpui::App) {
        if let Some(previous) = self.holes_on.take() {
            if current.as_ref().map(|c| c.entity_id()) != Some(previous.entity_id()) {
                if let Some(previous) = previous.upgrade() {
                    previous.update(cx, |state, _| state.set_holes(Vec::new()));
                }
            }
        }
        if let Some(state) = current {
            state.update(cx, |state, _| state.set_holes(holes));
            self.holes_on = Some(state.downgrade());
        }
    }
}

impl BrowserView {
    /// The frame's one occlusion decision (see the module doc): what shows,
    /// the centre page's stored rect, every live page's obscured flag, and
    /// the frame-edge rules that read the answer (the chrome popovers'
    /// honesty, the page taking the keyboard, the Split's focus, the Peek).
    /// In memory only: no tab write and no `browser.db` call here — the
    /// page reconcile owns those (`sync.rs`).
    pub(crate) fn occlude(&mut self, covers: Covers, cx: &mut Context<Self>) -> PageShow {
        let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        // Who holds the keyboard, once per frame: both the pane's and the
        // centre's liveness read it, and only the really shown page ever
        // counts (see below).
        let holds: std::collections::HashMap<TabId, bool> =
            self.states.iter().map(|(id, state)| (*id, state.read(cx).holds_keyboard())).collect();
        // The Baaz-drawn crashed page covers the centre page's rect. The
        // chrome popovers are resolved below, once the centre tab is known.
        let page_error_open = self
            .browse_centre_shown(cx)
            .and_then(|id| self.states.get(&id))
            .is_some_and(|state| state.read(cx).crashed().is_some());
        let base = BrowserVisibility {
            centre_browse: false,
            session_active: false,
            drop_cover: self.drop_cover,
            chrome_popover_open: false,
            page_error_open,
            ..covers.base
        };
        // The frame shows a page exactly when the pane stands open on the
        // Browser kind with nothing over it; ownership rides `pane_tab`,
        // which is the frame owner's active tab (or nothing while pending).
        // D6 (docs/43 §3): while the pane's strip slides, and where the Page
        // screen draws Baaz's own state (no page yet, couldn't load, a
        // pushed view), the pane covers its page (the region contract's
        // `covers_page`).
        let frame_live = browser_visible(BrowserVisibility { session_active: true, ..base }) && !covers.pane_covered;
        // B2a-H2b: in Browse the pane never shows a page — one tab, one
        // page — so the session page can neither share the centre's tab
        // nor paint behind the mode. The render path shows the Changes
        // fallback instead, and every pane page hides here.
        let (shown, frame_live) = if in_browse { (None, false) } else { (covers.pane_tab, frame_live) };
        // The Page float anchors to the pane's page: when its rect moved, the
        // float follows after this frame (`BrowserEvent::PageRectMoved`).
        let pane_page_rect = shown.and_then(|id| self.states.get(&id)).and_then(|state| state.read(cx).page_rect());
        if self.pane_page_rect != pane_page_rect {
            self.pane_page_rect = pane_page_rect;
            cx.emit(BrowserEvent::PageRectMoved);
        }
        // B2a-H2: in Browse the centre shows its tab's page under what the
        // main window draws over it (see `centre_visibility`); the Skills
        // page and a pending provider switch hide it too.
        let centre_shown = self.browse_centre_shown(cx);
        // B2a-H5: a switch dismisses the chrome popovers (they point at
        // the old tab's state), and leaving Browse closes them all; the
        // find strip is per tab and stays. The last-seen tab stores every
        // frame — including from `None` — so the switch check is never
        // dead and no flag leaks into Sessions mode. The first sight sets
        // without dismissing, so a `--steps` popover opens onto its own
        // first frame.
        let switched = self.last_centre_shown.is_some() && self.last_centre_shown != centre_shown;
        if switched {
            // Every route to a new centre tab (close, ⌘T, popup, omnibox,
            // task folder) — not only a sidebar click — moves find: the
            // tab left stops highlighting, and the shared input picks up
            // the new tab's query from the root, which holds the mutable
            // window that `set_value` needs (`BrowserEvent::CentreSwitched`,
            // delivered after this frame).
            cx.emit(BrowserEvent::CentreSwitched { left: self.last_centre_shown, shown: centre_shown });
        }
        if switched || !in_browse {
            self.overlays.update(cx, |o, cx| o.close_all(&[LayerKind::Downloads], cx));
        }
        // However the downloads popover closed (a switch, its toggle,
        // Escape off the overlay stack), it has not rendered for its next
        // opening.
        if !self.overlays.read(cx).stack.is_open(LayerKind::Downloads) {
            self.downloads_rendered = false;
        }
        self.last_centre_shown = centre_shown;
        // B2a-H5: covering is honest — a popover covers the page only while
        // it actually renders. A flag whose popover cannot render (no
        // downloads, no shown tab) clears instead of blanking the page
        // under an invisible popover; the downloads flag clears only once
        // it rendered, so a scripted download's first frame never closes
        // its own capture.
        let downloads_empty = self.browse_downloads(cx).is_empty();
        if self.overlays.read(cx).stack.is_open(LayerKind::Downloads) && centre_shown.is_some() && !downloads_empty {
            self.downloads_rendered = true;
        }
        // The footer's popover (anchored) shows an empty list as such.
        if self.overlays.read(cx).stack.is_open(LayerKind::Downloads)
            && self.downloads_anchor.is_none()
            && chrome::downloads_flag_clears(centre_shown.is_some(), downloads_empty, self.downloads_rendered)
        {
            self.overlays.update(cx, |o, cx| { o.close(LayerKind::Downloads, cx); });
            self.downloads_rendered = false;
        }
        let chrome_popover_open = in_browse
            && self.overlays.read(cx).stack.is_open(LayerKind::Downloads)
            && !self.downloads_overlay_wanted(cx)
            && centre_shown.is_some()
            && !self.browse_downloads(cx).is_empty();
        // A crashed page hides the find strip: the highlight stops and the
        // strip closes, so the error page owns the rect alone. First, so
        // the stored rect below never reserves a row for a strip that just
        // closed.
        if let Some(id) = centre_shown {
            if let Some(state) = self.states.get(&id).cloned() {
                if state.read(cx).crashed().is_some() && self.find_strips.for_tab(id).close() {
                    state.update(cx, |state, _| state.stop_find());
                }
            }
        }
        // The find and permission strips are in-flow rows above the page,
        // so the stored page rect starts below them: placement and the
        // mouse-down release share this one rect.
        let find_open = self.find_strips.visible(centre_shown).is_some();
        let permission_open = centre_shown
            .and_then(|id| self.states.get(&id))
            .is_some_and(|state| !state.read(cx).permissions().is_empty());
        let stack = chrome::strip_stack_px(find_open, permission_open);
        let mut centre_page = covers.centre_rect;
        if in_browse && stack > 0.0 {
            centre_page.origin.y += gpui::px(stack);
            centre_page.size.height = (centre_page.size.height - gpui::px(stack)).max(gpui::px(0.0));
        }
        // The floats over the page anchor to this rect: when it moved, they
        // follow after this frame (`BrowserEvent::PageRectMoved`).
        let centre_page_rect = in_browse.then_some(centre_page);
        if self.centre_page_rect != centre_page_rect {
            self.centre_page_rect = centre_page_rect;
            cx.emit(BrowserEvent::PageRectMoved);
        }
        let centre_live = browser_visible(centre_visibility(
            base,
            centre_shown.is_some(),
            centre_shown.is_some() && covers.centre_allowed,
            chrome_popover_open,
        ));
        // Editing ends when the page takes the keyboard (B2a-H4 7b): a click
        // into a Chromium page moves no gpui focus, so the address input
        // never blurs and would keep its editing look. The frame ends it
        // instead — on the rising edge only, so typing in the field (which
        // the page never holds through) never cancels itself.
        // Only a page the person can see and click counts: creating a tab's
        // (hidden, blank) Chromium window makes it key for a moment, which
        // dropped the new-tab field ⌘T had just focused (B2a live pass).
        let centre_blank = centre_shown
            .and_then(|id| self.tabs.get(id))
            .is_none_or(|tab| tab.url.is_empty() || tab.url == BLANK);
        let centre_holds = centre_live && !centre_blank && centre_shown.and_then(|id| holds.get(&id).copied()).unwrap_or(false);
        if in_browse && centre_holds && !self.browse_last_holds {
            // The page just took the keyboard: drop the field with no
            // refocus (the frame owns no `&mut Window` here, and handing
            // focus anywhere would steal it straight back from the page).
            self.drop_browse_address(cx);
        }
        self.browse_last_holds = centre_holds;
        self.apply_page_visibility(shown, frame_live, centre_shown, centre_live, cx);
        // R4: a Split partner shows beside the centre; a Peek shows over it,
        // the centre (and its partner) frozen into their stand-ins beneath.
        let partner = if in_browse { self.split_partner(cx) } else { None };
        let peeking = in_browse && self.peek.is_some();
        // A click into the other half (it just took the keyboard) makes it
        // the shown tab: the rising edge, since the left page may still
        // report holding it.
        let partner_holds = partner.is_some_and(|p| holds.get(&p).copied().unwrap_or(false));
        // The page script reports a press (`take_clicked`): read both
        // halves every frame so an old press never counts later.
        let centre_clicked = centre_shown
            .and_then(|id| self.states.get(&id).cloned())
            .is_some_and(|state| state.update(cx, |state, _| state.take_clicked()));
        let partner_clicked = partner
            .and_then(|id| self.states.get(&id).cloned())
            .is_some_and(|state| state.update(cx, |state, _| state.take_clicked()));
        if let Some(partner) = partner {
            let rising = partner_holds && !self.partner_held;
            if centre_live && !peeking && (partner_clicked && !centre_clicked || rising) {
                crate::baaz_log!("browse: split focus → tab {}", partner.0);
                // The root shows it, after this frame (it selects with the
                // window in hand).
                cx.emit(BrowserEvent::SplitFocus(partner));
            }
        }
        self.partner_held = partner_holds;
        if let Some(peek) = self.peek.as_ref() {
            let state = peek.state.clone();
            state.update(cx, |state, cx| state.set_obscured(!(peeking && centre_live), cx));
        }
        if !in_browse && self.peek.is_some() {
            self.close_peek(cx);
        }
        // The shown pages stamp their tabs when they really show (never a
        // blank), so discard measures real hiding — in memory only, never
        // a write on its own.
        for (id, live) in [(shown, frame_live), (centre_shown, centre_live)] {
            if live {
                if let Some(id) = id {
                    let blank = self
                        .states
                        .get(&id)
                        .map(|state| matches!(state.read(cx).url().as_ref(), "" | BLANK))
                        .unwrap_or(true);
                    if !blank {
                        self.tabs.mark_shown(id, tabs::now_ms());
                    }
                }
            }
        }
        let answer = PageShow {
            pane: shown,
            pane_live: frame_live,
            centre: centre_shown,
            centre_live,
            centre_covered_by_view: in_browse && centre_shown.is_some() && (chrome_popover_open || page_error_open),
        };
        self.occlusion.shown = answer;
        answer
    }

    /// Set every live page's obscured flag from the shown tabs: the
    /// pane's page shows while the frame is live, Browse's centre page
    /// while the centre is live, every other page hides. Blank pages stay
    /// hidden (WKWebView paints `about:blank` white over the dark pane).
    pub(crate) fn apply_page_visibility(
        &mut self,
        shown: Option<TabId>,
        frame_live: bool,
        centre: Option<TabId>,
        centre_live: bool,
        cx: &mut gpui::App,
    ) {
        // R4: the Split partner shows with the centre; under a Peek both
        // freeze into their stand-ins (one decision per page per frame —
        // flipping a page live and back each frame never lets it settle).
        let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let partner = if in_browse { self.split_partner(cx) } else { None };
        let peeking = in_browse && self.peek.is_some();
        let pages: Vec<(TabId, Entity<WebviewState>)> = self.states.iter().map(|(id, state)| (*id, state.clone())).collect();
        for (id, state) in pages {
            let blank = matches!(state.read(cx).url().as_ref(), "" | BLANK);
            let in_centre = Some(id) == centre || Some(id) == partner;
            let visible = (frame_live && Some(id) == shown || centre_live && in_centre && !peeking) && !blank;
            state.update(cx, |state, cx| state.set_obscured(!visible, cx));
        }
    }

    /// Cut the shell's chrome over the centre (the sidebar peek) through
    /// the Browse page as holes, so it shows over the native page and takes
    /// the clicks there; none outside Browse. The Peek's and the Split's ×
    /// and the loading line ride with them.
    pub(crate) fn cut_holes(&mut self, rects: Vec<(Bounds<Pixels>, Pixels)>, cx: &mut gpui::App) {
        // R4: the Split's × on the shown half rides with the centre's holes.
        let in_browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let mut rects = rects;
        if in_browse {
            if let Some(hole) = self.peek_split_holes(cx) {
                rects.push(hole);
            }
        }
        let holes: Vec<[f32; 5]> = if in_browse {
            rects
                .iter()
                .chain(self.occlusion.progress_hole.get().map(|b| (b, Pixels::ZERO)).iter())
                .map(|(b, r)| [f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height), f32::from(*r)])
                .collect()
        } else {
            Vec::new()
        };
        // Holes belong to the page shown now: a page that stopped being the
        // centre (another tab, Sessions, Settings) gets its own back whole.
        let current = self.browse_centre_shown(cx).and_then(|id| self.states.get(&id).cloned());
        self.occlusion.hand_holes(holes, current, cx);
    }

    /// The × holes for this frame (window coordinates, radius): the Peek's
    /// on its own page, and each Split half's on its page. The centre page's
    /// hole is returned for [`Self::cut_holes`] to merge with its own.
    fn peek_split_holes(&mut self, cx: &mut gpui::App) -> Option<(gpui::Bounds<Pixels>, Pixels)> {
        let centre = self.centre_page_rect?;
        let r = gpui::px(CLOSE / 2.0);
        if let Some(peek) = self.peek.as_ref().map(|peek| peek.state.clone()) {
            let card = peek_split::peek_rect(centre);
            let hole = [peek_split::close_rect(card)].map(|b| [f32::from(b.origin.x), f32::from(b.origin.y), CLOSE, CLOSE, CLOSE / 2.0]);
            peek.update(cx, |state, _| state.set_holes(hole.to_vec()));
        }
        let pair = self.split?;
        let shown = self.browse_centre_shown(cx)?;
        let partner = self.split_partner(cx)?;
        let (left, right) = peek_split::split_rects(centre, self.split_ratio);
        let close = |half: gpui::Bounds<Pixels>| {
            gpui::Bounds::new(
                gpui::point(half.origin.x + half.size.width - gpui::px(CLOSE + CLOSE_INSET), half.origin.y + gpui::px(CLOSE_INSET)),
                gpui::size(gpui::px(CLOSE), gpui::px(CLOSE)),
            )
        };
        let (shown_rect, partner_rect) = if shown == pair.left { (close(left), close(right)) } else { (close(right), close(left)) };
        if let Some(state) = self.states.get(&partner).cloned() {
            let b = partner_rect;
            state.update(cx, |state, _| state.set_holes(vec![[f32::from(b.origin.x), f32::from(b.origin.y), CLOSE, CLOSE, CLOSE / 2.0]]));
        }
        Some((shown_rect, r))
    }

    /// Whether a just-covered page still waits for its picture (see
    /// [`NativeOcclusion::settling`]).
    pub(crate) fn browser_cover_settling(&self, cx: &gpui::App) -> bool {
        NativeOcclusion::settling(self.states.values(), cx)
    }
}

impl crate::app::Harness {
    /// What covers the native pages this frame, as only the root knows it
    /// (see [`Covers`]): the overlays it owns and the other regions'
    /// `covers_page` answers. The one place the root computes it.
    pub(crate) fn page_covers(&self, window: &gpui::Window, cx: &gpui::App) -> Covers {
        use crate::layout::{right_kind, Layout, RightKind};
        use crate::region::Region as _;
        // The Settings page covers the page while it stands (the region
        // contract's `covers_page`).
        let settings_open = self.settings_page.read(cx).covers_page(cx);
        // Every overlay answers for itself (plan 4.6): an entry covers the
        // page when it draws in the main window and says it covers. A modal
        // the overlay window hosts (Browse) sits above the live page and
        // covers nothing; so does every panel.
        let overlay_open = self.overlays.read(cx).stack.covers_page(cx);
        let kind = right_kind(Layout::get(cx));
        // Q4c: while a provider switch is pending no session's page shows —
        // `browser_key` still names the previous session, and its webview
        // must neither draw nor hit-test under the loading centre.
        let pending_switch = self.pending_right_neutral(cx);
        // The native-overlay rule: every page hides unless the frame shows
        // its owner's Browser kind with nothing over it — and only the
        // shown tab's page ever shows. A tab is its owner's page exactly
        // when its owner is the frame's owner.
        let active_key = self.browser_key(cx);
        let browser = self.browser.read(cx);
        let shown_owner = browser.tab_owner_for_key(&active_key);
        let shown_key = match &shown_owner {
            super::Owner::Session(session) => session.clone(),
            super::Owner::Person => super::HOME_KEY.to_owned(),
        };
        // Only its owner's page ever shows — and, while a provider switch
        // is pending, not even that one.
        let shown_space = browser.owner_space(&shown_owner);
        let pane_tab = super::browser_session_active(&shown_key, &active_key, pending_switch)
            .then(|| browser.tabs.active_of(&shown_owner, shown_space.as_deref()))
            .flatten();
        let base = BrowserVisibility {
            pane_open: Layout::get(cx).right_open,
            kind_browser: kind == RightKind::Browser,
            centre_browse: false,
            session_active: false,
            settings_open,
            overlay_open,
            drop_cover: false,
            // The Field draws its rows in the overlay window, over the live
            // page: only the in-window dropdowns still cover it.
            omnibox_open: self.browse_omnibox_open(cx) && !self.field_overlay_wanted(cx),
            chrome_popover_open: false,
            page_error_open: false,
            // No "window active" input: the page is a child window of
            // ours, ordered with it, so another window taking the keyboard
            // (a menu-bar menu, DevTools, the page itself) covers nothing.
            // Hiding on deactivation blanked the page under every menu-bar
            // click and the Inspect window (owner screenshots 2026-10-11).
        };
        Covers {
            base,
            // D6: while the strip slides, and where the Page screen draws
            // Baaz's own state, the pane covers its page.
            pane_covered: self.right_pane.read(cx).covers_page(cx),
            pane_tab,
            // The Skills page and a pending provider switch hide the centre
            // exactly like the palette, menus, dialogs and settings hide the
            // pane's.
            // The Bookmarks and History page fills the centre like Skills.
            centre_allowed: self.pending_switch_dest(cx).is_none() && !self.skills_page.read(cx).covers_page(cx) && !self.library_open(cx),
            centre_rect: self.browse_centre_page_rect(super::window_content_bounds(window), window, cx),
        }
    }

    /// The frame's occlusion (Z7a, the native-overlay rule): hand what
    /// covers the pages to the one owner, which shows, parks and covers
    /// them. The page is composited above gpui, so gpui cannot draw over it
    /// — the host takes the view out instead. Decides only: the page
    /// reconcile (tabs, history, permissions, downloads) runs from the
    /// pages' own changes (`sync.rs`), never from here.
    pub(crate) fn occlude_pages(&mut self, window: &gpui::Window, cx: &mut gpui::Context<Self>) {
        let covers = self.page_covers(window, cx);
        self.browser.update(cx, |browser, cx| browser.occlude(covers, cx));
    }
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;

    /// The answer names the tab on screen: the centre's in Browse, else
    /// the pane's — what the done-task sweep and the discard spare.
    #[test]
    fn the_tab_on_screen_is_the_centre_before_the_pane() {
        let both = PageShow { pane: Some(TabId(1)), centre: Some(TabId(2)), ..PageShow::default() };
        assert_eq!(both.on_screen(), Some(TabId(2)));
        let pane = PageShow { pane: Some(TabId(1)), ..PageShow::default() };
        assert_eq!(pane.on_screen(), Some(TabId(1)));
        assert_eq!(PageShow::default().on_screen(), None);
    }
}
