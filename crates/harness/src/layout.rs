//! The sidebar width: the one piece of shell geometry the harness remembers.
//!
//! Global, not per workspace: the divider sits in the same place whatever the
//! window opened, the way the traffic-lights rail does. The file is
//! `~/Library/Application Support/harness/layout.json`, written atomically
//! through [`crate::store`], and every read is best-effort: a missing or
//! unparseable file is the default width, which loses a preference and never
//! a session.
//!
//! The drag math lives here too, next to the persistence, so both the pointer
//! intents and the tests share the one clamped expression.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What `layout.json` holds. `None` is "never resized": the default width.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Layout {
    /// The settled sidebar width in window pixels, if the person ever set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
}

/// `~/Library/Application Support/harness/layout.json`.
pub fn path() -> PathBuf {
    crate::store::support_dir().join("layout.json")
}

/// Read the store. Blocking; call it off the UI thread.
pub fn read() -> Layout {
    crate::store::read_json(&path())
}

/// Write the store, atomically.
///
/// Best-effort: a store that cannot be written loses a width, which is a
/// nuisance, and never a session, which would be a loss.
pub fn write(layout: &Layout) {
    if let Ok(text) = serde_json::to_vec_pretty(layout) {
        let _ = crate::store::write_atomic(&path(), &text);
    }
}

/// The width the shell should open at: the stored one, clamped into the
/// library range, or the default when nothing was ever stored.
pub fn sidebar_width(layout: &Layout) -> f32 {
    match layout.sidebar_width {
        Some(width) => aui::shell::clamp_sidebar_width(width),
        None => aui::shell::SIDEBAR_WIDTH,
    }
}

/// One drag move: the pointer travelled `x - grab_x` since the press, so the
/// divider follows from where it started, clamped into the library range.
pub fn drag_width(start_w: f32, grab_x: f32, x: f32) -> f32 {
    aui::shell::clamp_sidebar_width(start_w + (x - grab_x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_moves_the_divider_by_the_pointer_delta() {
        assert_eq!(drag_width(252.0, 100.0, 120.0), 272.0);
        assert_eq!(drag_width(252.0, 100.0, 80.0), 232.0);
    }

    #[test]
    fn a_drag_never_leaves_the_library_range() {
        assert_eq!(drag_width(252.0, 0.0, -10_000.0), aui::shell::SIDEBAR_MIN_WIDTH);
        assert_eq!(drag_width(252.0, 0.0, 10_000.0), aui::shell::SIDEBAR_MAX_WIDTH);
        assert_eq!(drag_width(180.0, 200.0, 100.0), aui::shell::SIDEBAR_MIN_WIDTH);
        assert_eq!(drag_width(420.0, 200.0, 300.0), aui::shell::SIDEBAR_MAX_WIDTH);
    }

    #[test]
    fn an_empty_store_opens_at_the_default_width() {
        assert_eq!(sidebar_width(&Layout::default()), aui::shell::SIDEBAR_WIDTH);
    }

    #[test]
    fn a_stored_width_is_clamped_on_the_way_in() {
        assert_eq!(
            sidebar_width(&Layout { sidebar_width: Some(1.0) }),
            aui::shell::SIDEBAR_MIN_WIDTH
        );
        assert_eq!(
            sidebar_width(&Layout { sidebar_width: Some(10_000.0) }),
            aui::shell::SIDEBAR_MAX_WIDTH
        );
        assert_eq!(sidebar_width(&Layout { sidebar_width: Some(300.0) }), 300.0);
    }
}
