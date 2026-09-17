//! The window's own preferences: the sidebar width, how the session list
//! groups, which groups stand closed, and how far search looks.
//!
//! Global, not per workspace: the divider sits in the same place whatever the
//! window opened, the way the traffic-lights rail does, and so does the
//! grouping — these are window preferences, not project ones. The file is
//! `~/Library/Application Support/harness/layout.json`, written atomically
//! through [`crate::store`], and every read is best-effort: a missing or
//! unparseable file is the defaults, which lose a preference and never
//! a session.
//!
//! The drag math lives here too, next to the persistence, so both the pointer
//! intents and the tests share the one clamped expression.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How the sidebar groups its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// Flat rows under calendar-day headers, as the window always did.
    Date,
    /// One collapsible group per project, then "Other workspaces".
    Project,
}

/// What `layout.json` holds. `None` is "never resized": the default width.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    /// The settled sidebar width in window pixels, if the person ever set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
    /// The explicit grouping choice. `None` is "undecided": the window reads
    /// Project once there is more than one adoption or a session that
    /// resolves to none, else Date — and persists the choice the first time
    /// the person toggles it.
    #[serde(rename = "groupBy", default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<GroupBy>,
    /// Group ids standing closed: project ids, and `"other"` for the
    /// "Other workspaces" group, which starts closed.
    #[serde(rename = "closedGroups", default, skip_serializing_if = "Vec::is_empty")]
    pub closed_groups: Vec<String>,
    /// Project-group ids whose held-back rows stand shown: the "Show N more"
    /// row's flip side. Persisted exactly like
    /// `closed_groups`, and empty by default so old files read unchanged.
    #[serde(rename = "expandedGroups", default, skip_serializing_if = "Vec::is_empty")]
    pub expanded_groups: Vec<String>,
    /// Whether the search palette looks across every project (`true`) or
    /// only the current one. Read by package 2's palette; stored here from
    /// the start so the toggle has somewhere to land.
    #[serde(rename = "searchAllProjects", default = "default_search_all")]
    pub search_all_projects: bool,
    /// Whether project group rows draw the collapse chevron in the leading
    /// box. Off by default: the row is a plain label.
    /// The Settings dialog's Sidebar section (⌘,) owns the switch; `--steps
    /// group-chevron` flips it for captures meanwhile.
    #[serde(rename = "groupChevron", default)]
    pub group_chevron: bool,
    /// Whether the current project wears its 2 px accent bar. Off by
    /// default: `current(true)` alone keeps only the semibold ink name (O4).
    /// Owned by the Settings dialog — see [`Self::group_chevron`].
    #[serde(rename = "groupBar", default)]
    pub group_bar: bool,
    /// Whether project group rows trail the workspace branch in mono. Off
    /// by default (O4). Owned by the Settings dialog — see
    /// [`Self::group_chevron`].
    #[serde(rename = "groupBranch", default)]
    pub group_branch: bool,
    /// Whether a first send earns one cheap model call for a generated
    /// title (auto-titles). ON by default; off means no model call ever
    /// happens for a title, and the row keeps its first-prompt label. The
    /// Settings dialog's Sidebar section owns the switch (part 4 wires it);
    /// the titler reads this field from part 2 on.
    #[serde(rename = "autoTitle", default = "default_true")]
    pub auto_title: bool,
    /// Whether a completed turn refreshes the sidebar's two-line byline
    /// from the transcript, spending a model call only when the free excerpt
    /// is poor (auto-summaries). ON by default; off means the ladder's
    /// preview rung only. Same ownership as [`Self::auto_title`].
    #[serde(rename = "autoSummary", default = "default_true")]
    pub auto_summary: bool,
}

/// The default for the auto-title and auto-summary switches: ON. A missing
/// key (every `layout.json` written before this feature) reads as enabled.
fn default_true() -> bool {
    true
}

fn default_search_all() -> bool {
    true
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: None,
            group_by: None,
            closed_groups: Vec::new(),
            expanded_groups: Vec::new(),
            search_all_projects: true,
            group_chevron: false,
            group_bar: false,
            group_branch: false,
            auto_title: true,
            auto_summary: true,
        }
    }
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
    fn the_new_preferences_default_and_round_trip() {
        let layout = Layout::default();
        assert_eq!(layout.group_by, None);
        assert!(layout.closed_groups.is_empty());
        assert!(layout.expanded_groups.is_empty());
        assert!(layout.search_all_projects);
        assert!(!layout.group_chevron);
        assert!(!layout.group_bar);
        assert!(!layout.group_branch);
        let stored = Layout {
            sidebar_width: Some(300.0),
            group_by: Some(GroupBy::Project),
            closed_groups: vec!["other".into()],
            expanded_groups: vec!["p-harness".into()],
            search_all_projects: false,
            group_chevron: true,
            group_bar: true,
            group_branch: true,
            auto_title: true,
            auto_summary: true,
        };
        let text = serde_json::to_string(&stored).unwrap();
        assert!(text.contains("\"groupBy\":\"project\""));
        assert!(text.contains("\"closedGroups\":[\"other\"]"));
        assert!(text.contains("\"expandedGroups\":[\"p-harness\"]"));
        assert!(text.contains("\"searchAllProjects\":false"));
        assert!(text.contains("\"groupChevron\":true"));
        assert!(text.contains("\"groupBar\":true"));
        assert!(text.contains("\"groupBranch\":true"));
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!(back.group_by, Some(GroupBy::Project));
        assert_eq!(back.closed_groups, vec!["other".to_owned()]);
        assert_eq!(back.expanded_groups, vec!["p-harness".to_owned()]);
        assert!(!back.search_all_projects);
        assert!(back.group_chevron);
        assert!(back.group_bar);
        assert!(back.group_branch);
        // An old file with only a width still reads, taking the new defaults.
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").unwrap();
        assert_eq!(old.group_by, None);
        assert!(old.expanded_groups.is_empty());
        assert!(old.search_all_projects);
        assert!(!old.group_chevron);
        assert!(!old.group_bar);
        assert!(!old.group_branch);
        // Both auto switches default ON, old files included: new behaviour
        // without a migration.
        assert!(Layout::default().auto_title);
        assert!(Layout::default().auto_summary);
        assert!(old.auto_title);
        assert!(old.auto_summary);
    }

    fn with_width(width: f32) -> Layout {
        Layout { sidebar_width: Some(width), ..Layout::default() }
    }

    #[test]
    fn a_stored_width_is_clamped_on_the_way_in() {
        assert_eq!(sidebar_width(&with_width(1.0)), aui::shell::SIDEBAR_MIN_WIDTH);
        assert_eq!(sidebar_width(&with_width(10_000.0)), aui::shell::SIDEBAR_MAX_WIDTH);
        assert_eq!(sidebar_width(&with_width(300.0)), 300.0);
    }
}
