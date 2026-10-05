//! Feedback mode core: the non-UI model, element probe, capture helpers and
//! report writer behind the owner's "point at the window and leave a note"
//! flow.
//!
//! A [`Draft`] collects [`Note`]s in memory while feedback mode is active;
//! on Done, [`report::write_report`] persists them as a Markdown + JSON
//! report folder for Claude. [`probe::MacProbe`] hit-tests the window
//! through macOS accessibility; [`capture`] crops and downsamples window
//! images; [`report`] renders the deterministic report text.
//!
//! F2/F3 build the UI on this API, so [`Draft::discard`], [`probe::MacProbe`]
//! and friends read as unused until then.

// F2/F3 own the UI on top of this API, so items that are only constructed
// or read there read as dead to this lane's compiler: silence that lint for
// the whole module rather than sprinkling per-item allows.
#![allow(dead_code)]

mod capture;
mod probe;
mod report;

// The flat re-export is the API F2/F3 are briefed against; until they land,
// the names no sibling uses read as unused imports.
#[allow(unused_imports)]
pub use capture::{crop_with_outline, render_window, to_logical};
#[allow(unused_imports)]
pub use probe::{enrich_from_debug_json, human_role, ElementProbe, FakeProbe, MacProbe};
#[allow(unused_imports)]
pub use report::{feedback_root, render_json, render_markdown, write_report};

use chrono::{DateTime, Local};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// A rectangle in window-logical pixels, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge in window-logical pixels.
    pub x: f32,
    /// Top edge in window-logical pixels.
    pub y: f32,
    /// Width in window-logical pixels.
    pub w: f32,
    /// Height in window-logical pixels.
    pub h: f32,
}

/// What the owner pointed at: an accessibility element enriched with its
/// gpui provenance, when known.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElementHit {
    /// The element's accessible name (may be empty when macOS names nothing).
    pub label: String,
    /// The raw macOS role, e.g. `AXButton`.
    pub role: String,
    /// Labelled ancestors nearest first as `(role, label)` pairs, stopping
    /// before `AXWindow`.
    pub ancestors: Vec<(String, String)>,
    /// The element's frame as a window-logical rect, top-left origin.
    pub rect: Rect,
    /// The gpui element id (the `Name("…")` wrapper stripped), when the
    /// accessibility tree could be matched to one.
    pub element_id: Option<String>,
    /// The Rust view path that built the element, when known.
    pub view: Option<String>,
}

/// What a note points at: either an element or a dragged box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Target {
    /// The owner clicked an accessibility element.
    Element(ElementHit),
    /// The owner dragged a box; `region` names it when they did.
    Area {
        /// The dragged box as a window-logical rect, top-left origin.
        rect: Rect,
        /// The owner's name for the boxed region, if they gave one.
        region: Option<String>,
    },
}

/// Where the app was when a note was taken. Plain data; F2 fills it in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenContext {
    /// Which screen the window was showing (sessions, browse, settings…).
    pub mode: String,
    /// The current project's display name, when there is one.
    pub project: Option<String>,
    /// The open session's id, when a session is open.
    pub session_id: Option<String>,
    /// The open session's title, when a session is open.
    pub session_title: Option<String>,
    /// What the right pane is showing, when anything is.
    pub right_pane: Option<String>,
    /// The right pane page URL, when the pane has one.
    pub page_url: Option<String>,
    /// The settings section in view, when on the settings screen.
    pub settings_section: Option<String>,
    /// Window width in logical pixels.
    pub window_w: f32,
    /// Window height in logical pixels.
    pub window_h: f32,
    /// The window's device-pixel scale factor (usually 2.0).
    pub scale: f32,
    /// The theme name (`dark` / `light`).
    pub theme: String,
}

/// One owner note: what they pointed at, what they said, and the images
/// held in memory until Done. Images are skipped in serde; everything else
/// round-trips.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// The note's 1-based position in the draft.
    pub index: usize,
    /// What the note points at.
    pub target: Target,
    /// The owner's words.
    pub text: String,
    /// Where the app was when the note was taken.
    pub context: ScreenContext,
    /// When the note was taken.
    #[serde(with = "local_time")]
    pub at: DateTime<Local>,
    /// The outlined crop around the target, at device pixels.
    #[serde(skip)]
    pub crop: Option<RgbaImage>,
    /// The full window shot, at device pixels.
    #[serde(skip)]
    pub full: Option<RgbaImage>,
}

/// The in-progress set of notes for one feedback session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    /// When the draft was started.
    #[serde(with = "local_time")]
    pub started: DateTime<Local>,
    /// The notes taken so far, in order.
    pub notes: Vec<Note>,
}

impl Draft {
    /// Start an empty draft now.
    pub fn new() -> Self {
        Self { started: Local::now(), notes: Vec::new() }
    }

    /// Push a note, assigning it the next 1-based index, and return that index.
    pub fn add(
        &mut self,
        target: Target,
        text: String,
        context: ScreenContext,
        crop: Option<RgbaImage>,
        full: Option<RgbaImage>,
    ) -> usize {
        let index = self.notes.len() + 1;
        self.notes.push(Note { index, target, text, context, at: Local::now(), crop, full });
        index
    }

    /// How many notes the draft holds.
    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// Whether the draft holds no notes.
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// Drop every note, keeping the draft's start time.
    pub fn discard(&mut self) {
        self.notes.clear();
    }
}

impl Default for Draft {
    /// An empty draft started now.
    fn default() -> Self {
        Self::new()
    }
}

/// Serde for `DateTime<Local>` as RFC 3339 text.
///
/// `chrono` here has no `serde` feature (adding it would change a shared
/// dependency), so the model carries its own two-function adapter instead.
mod local_time {
    use chrono::{DateTime, Local};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize the timestamp as RFC 3339 text.
    pub fn serialize<S: Serializer>(at: &DateTime<Local>, out: S) -> Result<S::Ok, S::Error> {
        out.serialize_str(&at.to_rfc3339())
    }

    /// Parse RFC 3339 text back into local time.
    pub fn deserialize<'de, D: Deserializer<'de>>(read: D) -> Result<DateTime<Local>, D::Error> {
        let raw = String::deserialize(read)?;
        raw.parse::<DateTime<chrono::FixedOffset>>()
            .map(|fixed| fixed.with_timezone(&Local))
            .map_err(serde::de::Error::custom)
    }
}

/// A minimal context for tests: every optional field empty, 2× dark window.
#[cfg(test)]
pub(crate) fn test_context() -> ScreenContext {
    ScreenContext {
        mode: "sessions".to_owned(),
        project: None,
        session_id: None,
        session_title: None,
        right_pane: None,
        page_url: None,
        settings_section: None,
        window_w: 1440.0,
        window_h: 900.0,
        scale: 2.0,
        theme: "dark".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_assigns_one_based_indices_in_order() {
        let mut draft = Draft::new();
        assert!(draft.is_empty());
        let first = draft.add(
            Target::Area { rect: Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 }, region: None },
            "first".to_owned(),
            test_context(),
            None,
            None,
        );
        let second = draft.add(
            Target::Area { rect: Rect { x: 5.0, y: 6.0, w: 7.0, h: 8.0 }, region: None },
            "second".to_owned(),
            test_context(),
            None,
            None,
        );
        assert_eq!((first, second), (1, 2));
        assert_eq!(draft.len(), 2);
        assert_eq!(draft.notes[0].index, 1);
        assert_eq!(draft.notes[1].index, 2);
    }

    #[test]
    fn discard_empties_the_draft_but_keeps_its_start() {
        let mut draft = Draft::new();
        let started = draft.started;
        draft.add(
            Target::Area { rect: Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, region: None },
            "gone".to_owned(),
            test_context(),
            None,
            None,
        );
        draft.discard();
        assert!(draft.is_empty());
        assert_eq!(draft.len(), 0);
        assert_eq!(draft.started, started);
    }

    #[test]
    fn notes_serde_round_trips_without_images() {
        let mut draft = Draft::new();
        draft.add(
            Target::Element(ElementHit {
                label: "New session".to_owned(),
                role: "AXButton".to_owned(),
                ancestors: vec![("AXGroup".to_owned(), "Sidebar".to_owned())],
                rect: Rect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 },
                element_id: Some("new-session".to_owned()),
                view: Some("baaz::app::Harness".to_owned()),
            }),
            "here".to_owned(),
            test_context(),
            None,
            None,
        );
        let json = serde_json::to_value(&draft).expect("serializes");
        assert_eq!(json["notes"][0]["target"]["Element"]["label"], "New session");
        let back: Draft = serde_json::from_value(json).expect("deserializes");
        assert_eq!(back, draft);
    }
}
