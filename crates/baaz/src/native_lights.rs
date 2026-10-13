//! The window's own traffic lights, hidden while the sidebar is closed and
//! back for the peek and the open column (owner 2026-10-13: closed shows
//! nothing, not even the lights). The lights belong to the sidebar's row, so
//! they go and come with it; nothing else in the app moves them.

/// Hides or shows the window's close / minimise / zoom buttons.
#[cfg(target_os = "macos")]
pub(crate) fn set_traffic_lights_hidden(window: &gpui::Window, hidden: bool) {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: the view is the live NSView gpui hands out for this window;
    // `window` and `standardWindowButton:` are plain getters, nil-checked,
    // and nothing is retained past this scope. Main thread (a render).
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        // NSWindowCloseButton = 0, NSWindowMiniaturizeButton = 1, NSWindowZoomButton = 2.
        for kind in 0usize..3 {
            let button: *mut AnyObject = msg_send![ns_window, standardWindowButton: kind];
            if !button.is_null() {
                let _: () = msg_send![button, setHidden: hidden];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_traffic_lights_hidden(_window: &gpui::Window, _hidden: bool) {}
