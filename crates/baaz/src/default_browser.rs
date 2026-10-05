//! The default-browser setting (B2a-H7): Settings → Browser → Default
//! browser.
//!
//! The row reads the handler through `LSCopyDefaultHandlerForURLScheme`
//! and sets it through `LSSetDefaultHandlerForURLScheme`, both through
//! framework links — no new crates. The pure pieces (which name a
//! handler id wears, the status line) are tested without touching the
//! system default; nothing here ever changes the default except the
//! row's own Make-default click, which the owner alone presses.

/// Baaz's bundle id, matching `scripts/bundle.sh`'s `Info.plist`.
pub(crate) const BUNDLE_ID: &str = "sh.baaz.app";

/// The name the status line wears for a handler id. Baaz and Safari get
/// their product names; anything else shows its bundle id, so an
/// unknown browser still reads as itself rather than as nothing.
pub(crate) fn display_name_for_handler(handler: &str) -> &str {
    match handler {
        BUNDLE_ID => "Baaz",
        "com.apple.Safari" => "Safari",
        _ => handler,
    }
}

/// The Default-browser row's status line for the handler currently
/// owning `https` (`None` when the lookup fails, e.g. off macOS).
pub(crate) fn status_line(handler: Option<&str>) -> String {
    match handler {
        Some(id) => format!("{} is your default browser", display_name_for_handler(id)),
        None => "Could not determine the default browser".to_owned(),
    }
}

/// Whether the row should offer Make default: anything but Baaz itself
/// (and the unreadable case, where trying again is the only way out).
pub(crate) fn offers_make_default(handler: Option<&str>) -> bool {
    !matches!(handler, Some(BUNDLE_ID))
}

#[cfg(target_os = "macos")]
#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn LSSetDefaultHandlerForURLScheme(
        scheme: *const std::ffi::c_void,
        handler: *const std::ffi::c_void,
    ) -> i32;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const std::ffi::c_void,
        c_str: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const std::ffi::c_void;
    fn CFStringGetCString(
        the_string: *const std::ffi::c_void,
        buffer: *mut std::ffi::c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// `kCFStringEncodingUTF8`, spelled out so no CoreFoundation header is
/// needed for the constant.
#[cfg(target_os = "macos")]
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// Wrap a Rust `&str` as a retained `CFStringRef` for one call.
#[cfg(target_os = "macos")]
struct CfString(*const std::ffi::c_void);

#[cfg(target_os = "macos")]
impl CfString {
    fn new(text: &str) -> Option<Self> {
        let bytes = std::ffi::CString::new(text).ok()?;
        let raw =
            unsafe { CFStringCreateWithCString(std::ptr::null(), bytes.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
        (!raw.is_null()).then_some(Self(raw))
    }
}

#[cfg(target_os = "macos")]
impl Drop for CfString {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

/// Read a retained handler `CFStringRef` into Rust text.
#[cfg(target_os = "macos")]
fn cf_string_to_rust(raw: *const std::ffi::c_void) -> Option<String> {
    let mut buffer = [0 as std::ffi::c_char; 1024];
    let ok = unsafe { CFStringGetCString(raw, buffer.as_mut_ptr(), buffer.len() as isize, K_CF_STRING_ENCODING_UTF8) };
    if ok == 0 {
        return None;
    }
    let len = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());
    let bytes: Vec<u8> = buffer[..len].iter().map(|byte| *byte as u8).collect();
    String::from_utf8(bytes).ok()
}

/// The bundle id currently owning the `https` scheme, or `None` when
/// the lookup fails. Reads only — never changes the default.
pub(crate) fn default_handler_for_https() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let scheme = CfString::new("https")?;
        let raw = unsafe { LSCopyDefaultHandlerForURLScheme(scheme.0) };
        if raw.is_null() {
            return None;
        }
        let handler = cf_string_to_rust(raw);
        unsafe { CFRelease(raw) };
        handler
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::baaz_log!("default browser: lookup is macOS-only");
        None
    }
}

/// Make Baaz the handler for `http` and `https`. macOS shows its own
/// confirmation; true only when both calls succeed. Called solely from
/// the Settings row's Make-default click — never from a test or a run.
pub(crate) fn make_baaz_default() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(handler) = CfString::new(BUNDLE_ID) else { return false };
        let mut ok = true;
        for scheme_name in ["http", "https"] {
            let Some(scheme) = CfString::new(scheme_name) else { return false };
            let status = unsafe { LSSetDefaultHandlerForURLScheme(scheme.0, handler.0) };
            if status != 0 {
                crate::baaz_log!("default browser: LSSetDefaultHandlerForURLScheme({scheme_name}) status={status}");
                ok = false;
            }
        }
        ok
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::baaz_log!("default browser: Make default is macOS-only");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baaz_and_safari_wear_product_names() {
        assert_eq!(display_name_for_handler("sh.baaz.app"), "Baaz");
        assert_eq!(display_name_for_handler("com.apple.Safari"), "Safari");
    }

    #[test]
    fn an_unknown_browser_shows_its_bundle_id() {
        assert_eq!(display_name_for_handler("com.example.other"), "com.example.other");
    }

    #[test]
    fn the_status_line_names_the_current_default() {
        assert_eq!(status_line(Some("sh.baaz.app")), "Baaz is your default browser");
        assert_eq!(status_line(Some("com.apple.Safari")), "Safari is your default browser");
        assert_eq!(
            status_line(Some("com.example.other")),
            "com.example.other is your default browser"
        );
    }

    #[test]
    fn an_unreadable_default_still_reads_as_a_line() {
        assert_eq!(status_line(None), "Could not determine the default browser");
    }

    #[test]
    fn make_default_is_offered_until_baaz_owns_it() {
        assert!(offers_make_default(Some("com.apple.Safari")));
        assert!(offers_make_default(None));
        assert!(!offers_make_default(Some("sh.baaz.app")));
    }
}
