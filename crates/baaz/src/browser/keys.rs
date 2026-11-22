//! Keyboard and wheel routing into the native page (task 4.5 split of
//! `browser/mod.rs`): the outside-click hand-back and the CEF key and wheel
//! routers.

use super::*;
use crate::layout::LayoutGlobal as _;

impl Harness {

    /// Capture-phase mouse-down anywhere in the window: a click outside
    /// the browser page's rect hands the keyboard back when the page
    /// holds it (the WKWebView keeps the NSWindow first responder after
    /// one click in the page, which used to kill typing app-wide until
    /// relaunch). Never consumes the event — the click still reaches its
    /// target. No-ops unless the right pane shows the Browser, and never
    /// creates a webview as a side effect.
    pub(crate) fn release_browser_keyboard_on_mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // In Browse the keyboard sits with the centre page, whose rect
        // is the stored centre rect — the very rect the frame's occlusion
        // placed — never the pane's and never a second computation. Before
        // the first frame the live computation stands in.
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            let Some(state) = self.browse_centre_state(cx) else {
                return;
            };
            let page = match self.browser.read(cx).centre_page_rect {
                Some(page) => page,
                None => {
                    let content = window_content_bounds(window);
                    self.browse_centre_page_rect(content, window, cx)
                }
            };
            if should_release_keyboard_on_mouse_down(
                state.read(cx).holds_keyboard(),
                event.position,
                Some(page),
            ) {
                state.update(cx, |state, _| state.release_keyboard());
            }
            return;
        }
        if !browser_pane_showing(
            crate::layout::Layout::get(cx).right_open,
            crate::layout::right_kind(crate::layout::Layout::get(cx)) == RightKind::Browser,
        ) {
            return;
        }
        let Some(state) = self.existing_browser(cx) else {
            return;
        };
        let content = window_content_bounds(window);
        // The live page sits under the pane's tab row and nav row: a press
        // on either is outside the page and takes the keyboard back too.
        let page = browser_page_bounds(content, gpui::px(self.right_width(cx)));
        if should_release_keyboard_on_mouse_down(
            state.read(cx).holds_keyboard(),
            event.position,
            Some(page),
        ) {
            state.update(cx, |state, _| state.release_keyboard());
        }
    }

    /// Deliver one keystroke routed out of a focused Chromium page: first
    /// hand the keyboard back to the gpui window through the existing
    /// hand-back path (the active browser's `release_keyboard`), then
    /// dispatch the keystroke into the window — so ⌘L ends with the address
    /// field focused, exactly as if gpui had seen the key. Blocked Chromium
    /// command ids drained here are logged for later mapping.
    ///
    /// Not behind the `cef` feature alone: the delivery (hand-back plus
    /// deferred dispatch) is plain gpui, and the regression test drives it
    /// without a page. Only the blocked-command drain needs CEF.
    #[cfg(any(all(test, feature = "probe"), feature = "cef"))]
    pub(crate) fn deliver_cef_keystroke(
        &mut self,
        keystroke: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // R4: Esc closes the Peek and ⌘↩ makes it a tab, from inside it.
        if self.browser.read(cx).peek.is_some() {
            match keystroke {
                "escape" => return self.browser.update(cx, |browser, cx| browser.close_peek(cx)),
                "cmd-enter" => return self.peek_to_tab(cx),
                _ => {}
            }
        }
        if let Some(state) = self.existing_browser(cx) {
            state.update(cx, |state, _| state.release_keyboard());
        }
        // In Browse the keyboard may sit with the centre page instead.
        if let Some(state) = self.browse_centre_state(cx) {
            state.update(cx, |state, _| state.release_keyboard());
        }
        #[cfg(feature = "cef")]
        for id in aui_cef::take_blocked_commands() {
            crate::baaz_log!("browser: blocked Chromium command {id}");
        }
        match crate::keymap::parse_keystroke(keystroke) {
            Ok(parsed) => {
                crate::baaz_log!("browser: page routed {keystroke}");
                // Deferred, never inline: `dispatch_keystroke` runs action
                // listeners synchronously, and every Harness listener
                // re-enters Harness — dispatching here would nest an update
                // inside this one and panic ("already being updated").
                // `Window::defer` runs the dispatch after this update's
                // lease on Harness has ended.
                // And only once AppKit has made the gpui window key again:
                // handing the keyboard back is asynchronous, and a dispatch
                // that lands first has its focus (⌘L's address field) reset
                // when the window's key change arrives (B1b, live: the second
                // ⌘L after clicking the page opened nothing).
                cx.spawn_in(window, async move |this, cx| {
                    for _ in 0..20 {
                        let key = cx
                            .update(|window, _| window.is_window_active())
                            .unwrap_or(true);
                        if key {
                            break;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(10))
                            .await;
                    }
                    // With gpui focus stale (the address field unmounted when
                    // the page took the keyboard) a keystroke reaches no key
                    // context and no binding: the second ⌘L after clicking the
                    // page did nothing (B1b, live). Focus the root first so
                    // `AuiRoot` bindings resolve. The handle is cloned out
                    // first: dispatching inside a Harness update re-enters it.
                    let root = this.update(cx, |this, _| this.focus_root.clone()).ok();
                    let _ = cx.update(|window, cx| {
                        // Unconditionally: the keystroke came from a page, so
                        // whatever gpui still calls focused is stale — often the
                        // unmounted address input, which `focused()` reports
                        // but no rendered node carries, so it matched nothing.
                        if let Some(root) = &root {
                            window.focus(root, cx);
                        }
                        window.dispatch_keystroke(parsed, cx);
                    });
                })
                .detach();
            }
            Err(_) => {
                crate::baaz_log!("browser: ignoring unparseable routed keystroke {keystroke:?}");
            }
        }
    }

    /// Install the Chromium key router for this window: `claims` is membership
    /// in the current claimed-set snapshot (a thread-local, never gpui state,
    /// which the CEF callback must not borrow) and `deliver` posts into a
    /// channel that one task drains through [`Self::deliver_cef_keystroke`].
    /// Still called where each shell window is created (`open_shell_window`),
    /// never from `cef_backend` — the router must exist before the first page
    /// takes the keyboard — but installed once per process: a second window
    /// re-installing would let its close strand every claimed key. Delivery
    /// resolves the window per keystroke (the active shell window), so the
    /// installing window closing never strands the rest.
    #[cfg(feature = "cef")]
    pub(crate) fn install_cef_key_router(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        adblock::start(&crate::layout::Layout::get(cx).adblock);
        aui_cef::set_key_router(|k| crate::keymap::claims_keystroke(k) || peek_split::peek_claims(k), move |keystroke| {
            let _ = tx.unbounded_send(keystroke);
        });
        // One task awaiting the channel — no polling timer, no idle wakeups.
        // A `spawn`, not a `spawn_in`: the drain resolves the window on every
        // keystroke instead of pinning the installing one.
        let task = cx.spawn(async move |weak, cx| {
            use futures::StreamExt as _;
            while let Some(keystroke) = rx.next().await {
                let Some(harness) = weak.upgrade() else {
                    crate::baaz_log!(
                        "browser: routed {keystroke} after the window closed; dropping"
                    );
                    break;
                };
                cx.update(|cx| {
                    let Some(target) = cx.active_window() else {
                        crate::baaz_log!(
                            "browser: routed {keystroke} with no live window; dropping"
                        );
                        return;
                    };
                    let _ = target.update(cx, |_, window, cx| {
                        harness.update(cx, |this, cx| {
                            this.deliver_cef_keystroke(&keystroke, window, cx);
                        });
                    });
                });
            }
        });
        self.tasks.push(task);
        self.install_cef_wheel_router(cx);
    }

    /// The native wheel router (D6 follow-up; docs/43 §3): the right pane's
    /// page is a child window, so a Shift + sideways swipe over it went to
    /// the page and the strip never moved. The shim's monitor asks
    /// [`page_wheel_claimed`] (plain state only); a claimed swipe is queued
    /// and replayed into the active window as the wheel event it is, so
    /// the pane's own Shift-swipe listener moves the strip unchanged.
    #[cfg(feature = "cef")]
    pub(super) fn install_cef_wheel_router(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<aui_cef::PageWheel>();
        aui_cef::set_wheel_router(page_wheel_claimed, move |wheel| {
            let _ = tx.unbounded_send(wheel);
        });
        let task = cx.spawn(async move |_, cx| {
            use futures::StreamExt as _;
            while let Some(wheel) = rx.next().await {
                cx.update(|cx| {
                    let Some(target) = cx.active_window() else { return };
                    let _ = target.update(cx, |_, window, cx| {
                        window.dispatch_event(gpui::PlatformInput::Scroll(page_wheel_event(&wheel)), cx);
                    });
                });
            }
        });
        self.tasks.push(task);
    }
}
