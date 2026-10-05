//! The default-browser setting (B2a-H7): Settings → Browser → Default
//! browser.
//!
//! The frame hook reads the handler through
//! `LSCopyDefaultHandlerForURLScheme` into the cache on [`Harness`](crate::app::Harness)
//! (focus-regain, the post-Make-default re-read, the first frame); the
//! row renders that cache only, and sets the handler through
//! `LSSetDefaultHandlerForURLScheme` — both through framework links, no
//! new crates. The pure pieces (which name a handler id wears, the
//! status line) are tested without touching the system default; nothing
//! here ever changes the default except the row's own Make-default
//! click, which the owner alone presses.

/// Baaz's bundle id, matching `scripts/bundle.sh`'s `Info.plist`.
pub(crate) const BUNDLE_ID: &str = "sh.baaz.app";

/// The name the status line wears for a handler id. Baaz, Safari and a
/// few common browsers get their product names; anything else shows its
/// bundle id, so an unknown browser still reads as itself rather than
/// as nothing. LaunchServices answers in lowercase (`com.apple.safari`),
/// so every comparison is case-insensitive.
pub(crate) fn display_name_for_handler(handler: &str) -> &str {
    if handler.eq_ignore_ascii_case(BUNDLE_ID) {
        return "Baaz";
    }
    if handler.eq_ignore_ascii_case("com.apple.safari") {
        return "Safari";
    }
    if handler.eq_ignore_ascii_case("com.google.chrome") {
        return "Chrome";
    }
    if handler.eq_ignore_ascii_case("org.mozilla.firefox") {
        return "Firefox";
    }
    if handler.eq_ignore_ascii_case("company.thebrowser.browser") {
        return "Arc";
    }
    if handler.eq_ignore_ascii_case("com.brave.browser") {
        return "Brave";
    }
    if handler.eq_ignore_ascii_case("com.microsoft.edgemac") {
        return "Edge";
    }
    handler
}

/// The Default-browser row's status line for the handler currently
/// owning `https` (`None` when the lookup fails, e.g. off macOS).
pub(crate) fn status_line(handler: Option<&str>) -> String {
    match handler {
        Some(id) => format!("{} is your default browser", display_name_for_handler(id)),
        None => "Could not determine the default browser".to_owned(),
    }
}

/// The row's status text: while macOS is still confirming a Make-default
/// click the row waits (`confirm_pending`), otherwise it names the live
/// handler. Pure, so the waiting state is pinned without touching the
/// system default.
pub(crate) fn status_text(handler: Option<&str>, confirm_pending: bool) -> String {
    if confirm_pending {
        return "Waiting for macOS to confirm…".to_owned();
    }
    status_line(handler)
}

/// The Default-browser row's fixed status line under determinism: captures
/// must never depend on the machine's handler.
pub(crate) const DETERMINISTIC_STATUS: &str = "Default browser: checked at runtime";

/// Whether captures must not depend on this machine (`BAAZ_DETERMINISTIC=1`,
/// which the capture harness always sets).
pub(crate) fn deterministic_captures() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// The Default-browser row's status line: the fixed capture text under
/// determinism, else [`status_text`].
pub(crate) fn row_status_text(handler: Option<&str>, confirm_pending: bool) -> String {
    if deterministic_captures() {
        return DETERMINISTIC_STATUS.to_owned();
    }
    status_text(handler, confirm_pending)
}

/// Whether the row should offer Make default: anything but Baaz itself
/// (and the unreadable case, where trying again is the only way out).
/// Case-insensitive, like the lookup answers.
pub(crate) fn offers_make_default(handler: Option<&str>) -> bool {
    !handler.is_some_and(|id| id.eq_ignore_ascii_case(BUNDLE_ID))
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

impl crate::app::Harness {
    /// The cached `https` handler for the Settings row: `None` until
    /// the first frame's read, or when the lookup fails. Render reads
    /// this only — never a live LaunchServices lookup.
    pub(crate) fn default_browser_handler(&self) -> Option<&str> {
        self.default_browser_cache.as_ref().and_then(|inner| inner.as_deref())
    }

    /// Re-read the `https` handler into the cache and retire the
    /// Make-default wait with its re-read. Read-only — never changes the
    /// default.
    pub(crate) fn refresh_default_browser_cache(&mut self) {
        self.default_browser_cache = Some(default_handler_for_https());
        self.default_browser_confirm_pending = false;
        self.default_browser_recheck_at = None;
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
    fn lowercase_handler_ids_still_wear_product_names() {
        // LaunchServices answers in lowercase; every id below is the
        // lowercase spelling the lookup returns.
        for (handler, name) in [
            ("sh.baaz.app", "Baaz"),
            ("com.apple.safari", "Safari"),
            ("com.google.chrome", "Chrome"),
            ("org.mozilla.firefox", "Firefox"),
            ("company.thebrowser.browser", "Arc"),
            ("com.brave.browser", "Brave"),
            ("com.microsoft.edgemac", "Edge"),
        ] {
            assert_eq!(display_name_for_handler(handler), name, "for {handler:?}");
            assert_eq!(
                status_line(Some(handler)),
                format!("{name} is your default browser"),
                "the status line for {handler:?}"
            );
        }
    }

    #[test]
    fn make_default_is_offered_case_insensitively() {
        assert!(!offers_make_default(Some("sh.baaz.app")));
        assert!(!offers_make_default(Some("SH.BAAZ.APP")));
        assert!(offers_make_default(Some("com.apple.safari")));
        assert!(offers_make_default(None));
    }

    #[test]
    fn the_row_waits_for_macos_to_confirm() {
        assert_eq!(
            status_text(Some("com.apple.safari"), true),
            "Waiting for macOS to confirm…"
        );
        assert_eq!(
            status_text(Some("com.apple.safari"), false),
            "Safari is your default browser"
        );
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

    #[test]
    fn deterministic_captures_wear_the_fixed_status_line() {
        // The variable is process-global: hold the store lock (which every
        // env-touching test holds) and restore whatever was there after.
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_DETERMINISTIC");
        assert_eq!(
            row_status_text(Some("com.apple.safari"), false),
            "Safari is your default browser",
            "live runs name the handler"
        );
        assert_eq!(
            row_status_text(None, false),
            "Could not determine the default browser",
            "live runs keep the unreadable line"
        );
        std::env::set_var("BAAZ_DETERMINISTIC", "1");
        assert!(deterministic_captures(), "the flag reads on");
        assert_eq!(
            row_status_text(Some("com.apple.safari"), false),
            "Default browser: checked at runtime",
            "captures never depend on the machine"
        );
        assert_eq!(
            row_status_text(None, true),
            "Default browser: checked at runtime",
            "captures wear it even while confirming"
        );
        std::env::set_var("BAAZ_DETERMINISTIC", "0");
        assert!(!deterministic_captures(), "any other value reads off");
        match old {
            Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
            None => std::env::remove_var("BAAZ_DETERMINISTIC"),
        }
        drop(guard);
    }
}
