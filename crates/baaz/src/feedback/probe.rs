//! Element probe: hit-test the Baaz window through macOS accessibility,
//! in-process, with no permission needed.
//!
//! gpui installs its AccessKit adapter on the NSWindow's **contentView**,
//! not on the raw view from the window handle, so hit-testing goes through
//! `[[view window] contentView]`. The first accessibility call only
//! activates the tree — it stays empty until gpui paints another frame — so
//! callers must [`ElementProbe::activate`], then `window.refresh()`, then
//! [`ElementProbe::hit`] on a later frame.
//!
//! [`MacProbe`] never panics: any nil object or failed conversion is `None`.
//! [`FakeProbe`] serves tests with a fixed hit list.

use super::ElementHit;
use serde_json::Value;

/// Hit-tests window points for the accessibility element under them.
pub trait ElementProbe {
    /// Touch the accessibility tree so it exists by the next frame. Call
    /// once, then `window.refresh()`, then [`ElementProbe::hit`] later.
    fn activate(&self, window: &mut gpui::Window);

    /// The element under `point` (window-logical pixels, top-left origin),
    /// or `None` when nothing labelled is there. Cheap: never enriches —
    /// hover calls this on every move, so the debug-tree serialise and
    /// parse waits for [`ElementProbe::enrich`] instead.
    fn hit(&self, window: &mut gpui::Window, point: gpui::Point<gpui::Pixels>) -> Option<ElementHit>;

    /// Fill `hit`'s gpui provenance (`element_id`, `view`) from the debug
    /// accessibility tree. Called once when a note's target is fixed, never
    /// for hover. The default is a no-op (the tree is macOS-only).
    fn enrich(&self, _window: &mut gpui::Window, _hit: &mut ElementHit) {}
}

/// The real macOS probe. On other targets [`ElementProbe::hit`] is `None`.
pub struct MacProbe;

impl ElementProbe for MacProbe {
    fn activate(&self, window: &mut gpui::Window) {
        #[cfg(target_os = "macos")]
        mac::activate(window);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
        }
    }

    fn hit(&self, window: &mut gpui::Window, point: gpui::Point<gpui::Pixels>) -> Option<ElementHit> {
        #[cfg(target_os = "macos")]
        {
            mac::hit(window, point)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            let _ = point;
            None
        }
    }

    fn enrich(&self, window: &mut gpui::Window, hit: &mut ElementHit) {
        #[cfg(target_os = "macos")]
        {
            if let Some(json) = window.debug_a11y_tree_json() {
                if let Some((element_id, view)) = enrich_from_debug_json(&json, &hit.label, &hit.role) {
                    hit.element_id = element_id;
                    hit.view = view;
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            let _ = hit;
        }
    }
}

/// A scripted probe for tests: the smallest-area hit containing the point.
pub struct FakeProbe {
    /// The hits to choose from, in any order.
    pub hits: Vec<ElementHit>,
    /// How many times [`ElementProbe::enrich`] ran: enrichment is free
    /// here (a no-op), so tests assert hover never enriches and a click
    /// enriches exactly once.
    pub enrich_calls: std::cell::Cell<usize>,
}

impl FakeProbe {
    /// The smallest-area hit containing `(x, y)`, in window-logical pixels.
    /// The [`ElementProbe`] impl delegates to this; tests call it directly
    /// so they need no window.
    pub fn hit_point(&self, x: f32, y: f32) -> Option<ElementHit> {
        self.hits
            .iter()
            .filter(|hit| {
                x >= hit.rect.x
                    && x <= hit.rect.x + hit.rect.w
                    && y >= hit.rect.y
                    && y <= hit.rect.y + hit.rect.h
            })
            .min_by(|a, b| {
                (a.rect.w * a.rect.h)
                    .partial_cmp(&(b.rect.w * b.rect.h))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
    }
}

impl ElementProbe for FakeProbe {
    fn activate(&self, _window: &mut gpui::Window) {}

    fn hit(&self, _window: &mut gpui::Window, point: gpui::Point<gpui::Pixels>) -> Option<ElementHit> {
        self.hit_point(f32::from(point.x), f32::from(point.y))
    }

    fn enrich(&self, _window: &mut gpui::Window, _hit: &mut ElementHit) {
        self.enrich_calls.set(self.enrich_calls.get() + 1);
    }
}

/// The human name for a macOS accessibility role, for reports.
pub fn human_role(ax: &str) -> &str {
    match ax {
        "AXButton" => "Button",
        "AXRadioButton" => "Radio button",
        "AXTabGroup" => "Tab group",
        "AXCheckBox" => "Checkbox",
        "AXTextField" | "AXTextArea" => "Text field",
        "AXRow" => "Row",
        "AXLink" => "Link",
        "AXPopUpButton" => "Pop-up button",
        _ => ax.strip_prefix("AX").unwrap_or(ax),
    }
}

/// Enrich a hit with its gpui provenance from `debug_a11y_tree_json` output.
///
/// Matches the node whose `aria.label` equals `label`, requiring the
/// `aria.role` to equal the accesskit role the `AX` role maps to — except
/// for unmapped `AX` roles, which match on the label alone. Ambiguous
/// matches (same label and role) resolve to the first node in document
/// order. Returns the stripped `element_id` (`Name("…")` unwrapped) and the
/// `view` path, each `None` when the node carries none. `None` when the
/// label is empty, the JSON does not parse, or nothing matches.
pub fn enrich_from_debug_json(json: &str, label: &str, role: &str) -> Option<(Option<String>, Option<String>)> {
    if label.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(json).ok()?;
    let nodes = parsed.get("nodes")?.as_object()?;
    let wanted = ax_accesskit_role(role);
    for node in nodes.values() {
        // A node without `aria` (structural nodes) is skipped, never fatal:
        // map order decides which node comes first.
        let Some(aria) = node.get("aria").and_then(Value::as_object) else {
            continue;
        };
        let node_label = aria.get("label").and_then(Value::as_str).unwrap_or("");
        if node_label != label {
            continue;
        }
        if let Some(roles) = wanted {
            let node_role = aria.get("role").and_then(Value::as_str).unwrap_or("");
            if !roles.contains(&node_role) {
                continue;
            }
        }
        let element_id =
            node.get("element_id").and_then(Value::as_str).map(strip_element_id).filter(|id| !id.is_empty());
        let view = node.get("view").and_then(Value::as_str).map(str::to_owned);
        return Some((element_id, view));
    }
    None
}

/// The accesskit role names an `AX` role can appear under in
/// `debug_a11y_tree_json`, or `None` to match on the label alone.
fn ax_accesskit_role(ax: &str) -> Option<&'static [&'static str]> {
    match ax {
        "AXButton" => Some(&["Button"]),
        "AXRadioButton" => Some(&["RadioButton"]),
        "AXTabGroup" => Some(&["TabList"]),
        "AXCheckBox" => Some(&["CheckBox"]),
        "AXTextField" => Some(&["TextInput"]),
        "AXRow" => Some(&["Row", "ListItem"]),
        _ => None,
    }
}

/// Unwrap the `Name("…")` wrapper the debug writer puts around gpui element
/// ids; anything else passes through unchanged.
fn strip_element_id(raw: &str) -> String {
    raw.strip_prefix("Name(\"")
        .and_then(|inner| inner.strip_suffix("\")"))
        .map(str::to_owned)
        .unwrap_or_else(|| raw.to_owned())
}

/// The raw macOS implementation. Every entry point checks for nil before
/// touching an object, so failures read as `None`, never a panic.
#[cfg(target_os = "macos")]
mod mac {
    use super::super::{ElementHit, Rect};
    use objc2::{msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{NSPoint, NSRect};

    /// Frames within this many points count as "the window's own frame".
    const FRAME_EPS: f64 = 1.0;
    /// The parent walk below never climbs further than this.
    const MAX_ANCESTORS: usize = 64;

    /// Touch the accessibility tree so it exists by the next frame.
    pub(super) fn activate(window: &gpui::Window) {
        let view = content_view(window);
        if view.is_null() {
            return;
        }
        unsafe {
            let _: Option<Retained<AnyObject>> = msg_send![&*view, accessibilityRole];
        }
    }

    /// The element under `point` (window-logical pixels, top-left origin).
    pub(super) fn hit(window: &mut gpui::Window, point: gpui::Point<gpui::Pixels>) -> Option<ElementHit> {
        let view = content_view(window);
        if view.is_null() {
            return None;
        }
        let window_ptr = unsafe {
            let win: *mut AnyObject = msg_send![&*view, window];
            win
        };
        if window_ptr.is_null() {
            return None;
        }
        // Window points are top-left origin in logical pixels; AppKit view
        // coordinates may be flipped, so mirror `y` unless the view says it
        // is already top-left.
        let bounds: NSRect = unsafe { msg_send![&*view, bounds] };
        let flipped: bool = unsafe { msg_send![&*view, isFlipped] };
        let in_view =
            NSPoint::new(f64::from(f32::from(point.x)), flipped_point(bounds, flipped, f64::from(f32::from(point.y))));
        let on_screen = unsafe {
            let in_window: NSPoint =
                msg_send![&*view, convertPoint: in_view, toView: std::ptr::null_mut::<AnyObject>()];
            let at: NSPoint = msg_send![&*window_ptr, convertPointToScreen: in_window];
            at
        };
        let found: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*view, accessibilityHitTest: on_screen] };
        let found: Retained<AnyObject> = found?;
        let raw = (&*found) as *const AnyObject as *mut AnyObject;
        let role = property(raw, Property::Role).unwrap_or_default();
        let label = property(raw, Property::Title)
            .or_else(|| property(raw, Property::Label))
            .or_else(|| property(raw, Property::Help))
            .or_else(|| property(raw, Property::Value))
            .unwrap_or_default();
        let frame: NSRect = unsafe { msg_send![&*raw, accessibilityFrame] };
        // A hit on an unlabelled point is the root group whose frame is the
        // window itself: that is "no element", not an element without a name.
        if label.is_empty() && role == "AXGroup" {
            let window_frame: NSRect = unsafe { msg_send![&*window_ptr, frame] };
            if frames_match(frame, window_frame) {
                return None;
            }
        }
        let rect = screen_to_logical(frame, window_ptr, view, bounds, flipped);
        let ancestors = parents(raw);
        // No enrichment here: hover hit-tests on every move, and the
        // debug-tree serialise plus parse costs a full frame. The shell
        // calls `enrich` once the note's target is fixed instead.
        Some(ElementHit { label, role, ancestors, rect, element_id: None, view: None })
    }

    /// Which `NSString` property to read.
    enum Property {
        /// `accessibilityTitle` — where labels arrive in this app.
        Title,
        /// `accessibilityLabel` — empty in the spike, kept as a fallback.
        Label,
        /// `accessibilityHelp`.
        Help,
        /// `accessibilityValue`.
        Value,
        /// `accessibilityRole`.
        Role,
    }

    /// Read one `NSString` property as Rust text; nil, empty or
    /// undecodable is `None`.
    fn property(obj: *mut AnyObject, which: Property) -> Option<String> {
        let kept: Option<Retained<AnyObject>> = unsafe {
            match which {
                Property::Title => msg_send![&*obj, accessibilityTitle],
                Property::Label => msg_send![&*obj, accessibilityLabel],
                Property::Help => msg_send![&*obj, accessibilityHelp],
                Property::Value => msg_send![&*obj, accessibilityValue],
                Property::Role => msg_send![&*obj, accessibilityRole],
            }
        };
        let text = cocoa_string(kept.as_deref()?)?;
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Copy an `NSString` to Rust through its UTF-8 bytes.
    fn cocoa_string(value: &AnyObject) -> Option<String> {
        // `accessibilityValue` is an NSNumber on checkboxes, radios and
        // sliders; `UTF8String` on one raises "unrecognized selector",
        // which aborts the app. Only NSStrings are read.
        let is_string: bool = unsafe { msg_send![value, isKindOfClass: objc2::class!(NSString)] };
        if !is_string {
            return None;
        }
        let bytes: *const std::ffi::c_char = unsafe { msg_send![value, UTF8String] };
        if bytes.is_null() {
            return None;
        }
        Some(unsafe { std::ffi::CStr::from_ptr(bytes).to_string_lossy().into_owned() })
    }

    /// Walk `accessibilityParent` from `child` collecting labelled
    /// `(role, label)` pairs nearest first, stopping before `AXWindow`.
    fn parents(child: *mut AnyObject) -> Vec<(String, String)> {
        let mut next: Option<Retained<AnyObject>> = unsafe { msg_send![&*child, accessibilityParent] };
        let mut out = Vec::new();
        for _ in 0..MAX_ANCESTORS {
            let parent = match next {
                Some(parent) => parent,
                None => break,
            };
            let raw = (&*parent) as *const AnyObject as *mut AnyObject;
            let role = property(raw, Property::Role).unwrap_or_default();
            if role == "AXWindow" {
                break;
            }
            let label = property(raw, Property::Title)
                .or_else(|| property(raw, Property::Label))
                .or_else(|| property(raw, Property::Help))
                .or_else(|| property(raw, Property::Value));
            if let Some(text) = label {
                out.push((role, text));
            }
            next = unsafe { msg_send![&*raw, accessibilityParent] };
        }
        out
    }

    /// The AccessKit content view behind a gpui window, or nil.
    fn content_view(window: &gpui::Window) -> *mut AnyObject {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = match HasWindowHandle::window_handle(window) {
            Ok(handle) => handle,
            Err(_) => return std::ptr::null_mut(),
        };
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return std::ptr::null_mut();
        };
        let view = appkit.ns_view.as_ptr() as *mut AnyObject;
        if view.is_null() {
            return std::ptr::null_mut();
        }
        let window_ptr: *mut AnyObject = unsafe { msg_send![&*view, window] };
        if window_ptr.is_null() {
            return std::ptr::null_mut();
        }
        unsafe {
            let content: *mut AnyObject = msg_send![&*window_ptr, contentView];
            content
        }
    }

    /// Mirror a top-left-origin `y` into view coordinates.
    fn flipped_point(bounds: NSRect, flipped: bool, y: f64) -> f64 {
        if flipped {
            y
        } else {
            bounds.size.height - y
        }
    }

    /// Map a screen-points frame back to a window-logical rect.
    fn screen_to_logical(
        frame: NSRect,
        window_ptr: *mut AnyObject,
        view: *mut AnyObject,
        bounds: NSRect,
        flipped: bool,
    ) -> Rect {
        let origin = unsafe {
            let in_window: NSPoint = msg_send![&*window_ptr, convertPointFromScreen: frame.origin];
            let in_view: NSPoint =
                msg_send![&*view, convertPoint: in_window, fromView: std::ptr::null_mut::<AnyObject>()];
            in_view
        };
        // `frame.origin` is the element's bottom-left corner in screen space.
        // In a flipped view that point is the element's bottom edge, so its
        // top sits one height above it.
        let y = if flipped { origin.y - frame.size.height } else { bounds.size.height - origin.y - frame.size.height };
        Rect { x: origin.x as f32, y: y as f32, w: frame.size.width as f32, h: frame.size.height as f32 }
    }

    /// Whether two screen frames are the same rect, within tolerance.
    fn frames_match(a: NSRect, b: NSRect) -> bool {
        (a.origin.x - b.origin.x).abs() <= FRAME_EPS
            && (a.origin.y - b.origin.y).abs() <= FRAME_EPS
            && (a.size.width - b.size.width).abs() <= FRAME_EPS
            && (a.size.height - b.size.height).abs() <= FRAME_EPS
    }
}

#[cfg(test)]
mod tests {
    use super::super::Rect;
    use super::*;

    /// A debug-tree sample in the shape gpui's field writer emits.
    fn sample_tree() -> &'static str {
        r#"{
            "root": "1",
            "frame": {},
            "nodes": {
                "1": {
                    "accesskit_id": "1",
                    "aria": {"role": "Group", "label": "Project harness. Open project menu"},
                    "children": ["2"]
                },
                "2": {
                    "accesskit_id": "2",
                    "element_id": "Name(\"hd-project\")",
                    "view": "baaz::app::Harness",
                    "aria": {"role": "Button", "label": "Project harness. Open project menu"}
                },
                "3": {
                    "accesskit_id": "3",
                    "element_id": "Name(\"other\")",
                    "view": "baaz::app::Other",
                    "aria": {"role": "RadioButton", "label": "Project harness. Open project menu"}
                }
            }
        }"#
    }

    #[test]
    fn enrichment_finds_the_code_id_by_label_and_role() {
        let found = enrich_from_debug_json(sample_tree(), "Project harness. Open project menu", "AXButton");
        assert_eq!(
            found,
            Some((Some("hd-project".to_owned()), Some("baaz::app::Harness".to_owned())))
        );
    }

    #[test]
    fn enrichment_disambiguates_by_role() {
        let found = enrich_from_debug_json(sample_tree(), "Project harness. Open project menu", "AXRadioButton");
        assert_eq!(found, Some((Some("other".to_owned()), Some("baaz::app::Other".to_owned()))));
    }

    #[test]
    fn enrichment_falls_back_to_label_only_for_unknown_roles() {
        let found = enrich_from_debug_json(sample_tree(), "Project harness. Open project menu", "AXUnknown");
        assert_eq!(found, Some((None, None)));
    }

    #[test]
    fn enrichment_returns_none_when_absent() {
        // A structural node without `aria` ahead of the match is skipped.
        let with_bare = r#"{"nodes":{"0":{"element_id":"Name(\"root\")"},"1":{"element_id":"Name(\"hd-project\")","view":"baaz::app::Harness","aria":{"role":"Button","label":"Open"}}}}"#;
        assert_eq!(
            enrich_from_debug_json(with_bare, "Open", "AXButton"),
            Some((Some("hd-project".to_owned()), Some("baaz::app::Harness".to_owned())))
        );
        assert_eq!(enrich_from_debug_json(sample_tree(), "No such label", "AXButton"), None);
        assert_eq!(enrich_from_debug_json(sample_tree(), "", "AXButton"), None);
        assert_eq!(enrich_from_debug_json("not json", "Project harness. Open project menu", "AXButton"), None);
    }

    fn hit_at(x: f32, y: f32, w: f32, h: f32, label: &str) -> ElementHit {
        ElementHit {
            label: label.to_owned(),
            role: "AXButton".to_owned(),
            ancestors: Vec::new(),
            rect: Rect { x, y, w, h },
            element_id: None,
            view: None,
        }
    }

    #[test]
    fn fake_probe_returns_the_innermost_containing_hit() {
        let probe = FakeProbe {
            hits: vec![
                hit_at(0.0, 0.0, 100.0, 100.0, "outer"),
                hit_at(10.0, 10.0, 20.0, 20.0, "inner"),
                hit_at(12.0, 12.0, 5.0, 5.0, "core"),
            ],
            enrich_calls: std::cell::Cell::new(0),
        };
        let label = |hit: Option<ElementHit>| hit.map(|hit| hit.label);
        assert_eq!(label(probe.hit_point(14.0, 14.0)), Some("core".to_owned()));
        assert_eq!(label(probe.hit_point(11.0, 11.0)), Some("inner".to_owned()));
        assert_eq!(label(probe.hit_point(50.0, 50.0)), Some("outer".to_owned()));
    }

    #[test]
    fn fake_probe_returns_none_outside_all_hits() {
        let probe = FakeProbe {
            hits: vec![hit_at(0.0, 0.0, 10.0, 10.0, "only")],
            enrich_calls: std::cell::Cell::new(0),
        };
        assert!(probe.hit_point(99.0, 99.0).is_none());
        assert!(
            FakeProbe { hits: vec![], enrich_calls: std::cell::Cell::new(0) }
                .hit_point(1.0, 1.0)
                .is_none()
        );
    }

    #[test]
    fn human_role_names_hold() {
        assert_eq!(human_role("AXButton"), "Button");
        assert_eq!(human_role("AXRadioButton"), "Radio button");
        assert_eq!(human_role("AXTabGroup"), "Tab group");
        assert_eq!(human_role("AXCheckBox"), "Checkbox");
        assert_eq!(human_role("AXTextField"), "Text field");
        assert_eq!(human_role("AXTextArea"), "Text field");
        assert_eq!(human_role("AXRow"), "Row");
        assert_eq!(human_role("AXLink"), "Link");
        assert_eq!(human_role("AXPopUpButton"), "Pop-up button");
        assert_eq!(human_role("AXMenu"), "Menu");
        assert_eq!(human_role("plain"), "plain");
    }
}
