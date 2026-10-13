//! The window's own traffic lights, hidden while the sidebar is closed and
//! back for the peek and the open column (owner 2026-10-13: closed shows
//! nothing, not even the lights). The lights belong to the sidebar's row, so
//! they go and come with it; nothing else in the app moves them.

/// Hides or shows the window's close / minimise / zoom buttons, then puts
/// them back where the shell's row wants them: AppKit lays the standard
/// buttons out again at their default (9, 9) whenever one's hidden state
/// changes, which undid gpui's placement and left the lights 6 pt above the
/// row's centre (owner 2026-10-13: "squished up"; traced with
/// `BAAZ_LIGHTS_TRACE=1`). The placement therefore follows the hiding.
#[cfg(target_os = "macos")]
pub(crate) fn set_traffic_lights_hidden(window: &gpui::Window, hidden: bool, cx: &gpui::App) {
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
    window.set_traffic_light_position(aui::shell::traffic_light_position(cx));
    if std::env::var_os("BAAZ_LIGHTS_TRACE").is_some() {
        // Where the close light really sits, from the window's top (AppKit's
        // frame is bottom-up): the number `traffic_light_position` is tuned to.
        // SAFETY: as above; getters only.
        unsafe {
            let view = handle.ns_view.as_ptr() as *mut AnyObject;
            let ns_window: *mut AnyObject = msg_send![view, window];
            let button: *mut AnyObject = msg_send![ns_window, standardWindowButton: 0usize];
            let cv: *mut AnyObject = msg_send![ns_window, contentView];
            if !button.is_null() && !cv.is_null() {
                let frame: objc2_foundation::NSRect = msg_send![button, frame];
                let sup: *mut AnyObject = msg_send![button, superview];
                let in_win: objc2_foundation::NSRect = msg_send![sup, convertRect: frame, toView: std::ptr::null::<AnyObject>()];
                let cvf: objc2_foundation::NSRect = msg_send![cv, frame];
                let centre = cvf.size.height - (in_win.origin.y + in_win.size.height / 2.0);
                eprintln!("baaz: lights: close frame origin ({}, {}), centre {centre:.1} pt from the top, hidden={hidden}", frame.origin.x, frame.origin.y);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_traffic_lights_hidden(_window: &gpui::Window, _hidden: bool, _cx: &gpui::App) {}
