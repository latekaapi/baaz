//! The Settings page (⌘,, File → Settings…, the account menu's
//! "Settings…" / "Providers…" rows, `--steps settings[:<section>]`,
//! palette, `settings:<section>` deep links).
//!
//! B11: Settings is a full page (`Route::Settings`), not a modal. While it
//! is open the left column shows the section list (with a "← Back" row at
//! its top) instead of the session sidebar; the centre+right columns merge
//! into one settings surface (content at most 720 px, centred, own scroll,
//! docs/83 §7). The right pane and terminal dock are hidden, not closed —
//! their state is untouched and restored on exit.
//!
//! The switches and the Appearance theme stay in `layout.json` — there is
//! no second store, and no copy of it: the layout is one gpui global
//! ([`crate::layout::LayoutStore`]). The page's rows read it every frame,
//! and a switch intent flips the matching layout field through
//! [`crate::layout::Layout::update`] (the root's
//! [`Harness::flip_setting`](crate::app::Harness::flip_setting), which also
//! applies what a switch changes elsewhere), which writes the file and
//! re-renders every view that observes the global.
//!
//! The page is a [`Region`] entity (task 4.4): [`SettingsPage`] owns its
//! route ([`SettingsPageState`]), its Shortcuts search field and focus, the
//! Shortcuts rows and recording, and every menu and fold on its pages, and
//! renders itself — the surface as its view, the section list
//! ([`SettingsPage::render_nav`]) and the crumbs
//! ([`SettingsPage::render_crumbs`]) for the root to seat in the sidebar
//! column and the bar, the close button as its [`Region::actions`]. What
//! only the root knows (the archived sessions, the default browser, history,
//! the move plan, the import offer, the provider new sessions start in)
//! reaches it as [`SettingsFacts`], pushed every frame and before every root
//! call into it (`Harness::with_settings_page`); the provider cards read the
//! shared [`ProviderStatusModel`](crate::provider_model::ProviderStatusModel)
//! (the Codex sign-in, what a card waits on), which the page observes. What
//! reaches past it goes out as a [`SettingsEvent`]. Others ask the entity
//! whether it stands open (`Harness::settings_open`), never a copy.
//!
//! B11b decisions, kept here so every caller agrees:
//!
//! * While the page stands open, pane/terminal toggles and pane-kind
//!   commands are IGNORED with no state change (never "close Settings,
//!   then act"): [`Harness::toggle_right`](crate::app::Harness::toggle_right),
//!   [`show_right`](crate::app::Harness::show_right),
//!   [`toggle_terminal`](crate::app::Harness::toggle_terminal),
//!   [`new_terminal`](crate::app::Harness::new_terminal),
//!   [`open_terminal_tab`](crate::app::Harness::open_terminal_tab), the
//!   matching palette actions and the `right:*` / `terminal-dock` /
//!   `files-select` step verbs all return early. The page hides the pane
//!   and dock without writing their state, so an ignored toggle keeps the
//!   restore exact.
//! * The page exits only when a navigation actually happens: every session
//!   switch lands through `activate`, which exits; a palette ACTION
//!   executed exits (except the ignored pane/terminal ones above).
//!   Merely opening the palette — or cancelling it — never exits.
//! * Opening arms the surface's focus (keyboard nav and ⌘F work at once);
//!   closing parks the composer next frame.
//!
//! Extensibility: a later section is one more id in
//! [`normalize_settings_target`] + [`SETTINGS_SECTIONS`], one more id in
//! [`apply_setting`]/[`setting_home`], and one more page arm in
//! [`SettingsPage::render_content`].
//!
//! The Shortcuts section is the exception to that shape: its rows come from
//! [`crate::keymap::effective_bindings`], cached on the page
//! ([`SettingsPage::shortcuts`], re-read on open and after every edit), and
//! its edits go through [`SettingsPage::handle_shortcut`].

use aui::data::{button, icon_button, section, segmented, settings_row, switch, ButtonSize, SwitchIntent, SETTINGS_ROW_PAD_Y};
use aui::interact::{interactive, list_row, LIST_INSET, ROW_GAP};
use aui::overlay::ShortcutEdit;
#[cfg(test)]
use aui::overlay::{SettingsRow, SettingsSection};
use aui::util::{h_flex, v_flex};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use std::collections::{HashMap, HashSet};

use gpui::{div, prelude::*, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Task, Window};
use gpui_kit::base::input::{InputEvent, TextareaState};

use crate::app::{Harness, SettingsFind, SETTINGS_CONTEXT};
use crate::region::Region;
use crate::browser::toolbar::SearchEngine;
use crate::layout::{browser_download_dir, tilde_path, Layout};
use crate::providers::ProviderId;
use crate::layout::DensityToken as _;
use crate::layout::ThemeToken as _;
use crate::layout::LayoutGlobal as _;
use crate::layout::TextSizeToken as _;
use crate::providers::ProviderMark as _;

/// Flip one Sidebar switch in `layout` by row id. `false` is an unknown id,
/// ignored by every caller. Pure, so tests drive it without a window;
/// [`Harness::flip_setting`] and the `auto-*` step verbs run it inside
/// [`crate::layout::Layout::update`], which persists and redraws.
pub(crate) fn apply_setting(layout: &mut Layout, id: &str, on: bool) -> bool {
    match id {
        "summary_line" => layout.summary_line = on,
        "group_branch" => layout.group_branch = on,
        "auto_title" => layout.auto_title = on,
        "auto_summary" => layout.auto_summary = on,
        "handoff_model_summary" => layout.handoff_model_summary = on,
        "fold_finished_turns" => layout.fold_finished_turns = on,
        "archive_on_merge" => layout.archive_on_merge = on,
        "notify_needs_you" => layout.set_notify(|n| n.needs_you = on),
        "notify_done" => layout.set_notify(|n| n.done = on),
        "notify_failed" => layout.set_notify(|n| n.failed = on),
        "notify_sound" => layout.set_notify(|n| n.sound = on),
        "notify_badge" => layout.set_notify(|n| n.badge = on),
        "adblock_enabled" => {
            layout.adblock.enabled = on;
            crate::browser::adblock::apply(&layout.adblock);
        }
        _ => return false,
    }
    true
}

/// What the Settings → Browser page's Chromium row opens: Chromium's own
/// settings surface, shown in a Browse person tab.
pub(crate) const CHROMIUM_SETTINGS_URL: &str = "chrome://settings";

/// Store the branch prefix as typed (normalised; blank or unusable is the
/// default, stored as nothing). Used by the General page's field.
pub(crate) fn set_worktree_branch_prefix(typed: &str, cx: &mut App) {
    let prefix = crate::layout::normalize_branch_prefix(typed)
        .filter(|prefix| prefix != baaz_core::worktrees::DEFAULT_BRANCH_PREFIX);
    if crate::layout::Layout::get(cx).worktree_branch_prefix != prefix {
        crate::layout::Layout::update(cx, |layout| layout.worktree_branch_prefix = prefix);
    }
}

/// The Settings content's widest measure (docs/83 §7: "content column max
/// 720"); the column adds its side padding, so its rows span at most this.
pub(crate) const SETTINGS_CONTENT_MAX: f32 = 720.0;
/// The Settings column's widest measure, centred in the surface: the
/// content plus its side padding. The Skills page takes the same column
/// (owner 2026-10-31: full width "looks too wide").
pub(crate) const SETTINGS_COLUMN_MAX: f32 = SETTINGS_CONTENT_MAX + 2.0 * SETTINGS_COLUMN_PAD_X;
/// The Shortcuts page's search field width.
const SHORTCUT_SEARCH_W: f32 = 280.0;
/// The Settings column's side padding.
pub(crate) const SETTINGS_COLUMN_PAD_X: f32 = scale::SP_5;

/// Where the Settings nav's Back row starts inside the sidebar column, under
/// the chrome row's line: the list inset (docs/83 §2.5), as the sidebar's own
/// rows start. The flat shell lays no fade over the column's top.
pub(crate) const SETTINGS_NAV_TOP: f32 = LIST_INSET;
/// Where the Settings column's heading starts under the centre header's
/// line (the column's content begins under the header); the Skills page's
/// header takes the same inset.
pub(crate) const SETTINGS_COLUMN_TOP: f32 = scale::SP_6;
/// The space between a page's description and its first section, and
/// between sections: a section reads as a block of its own, not as rows
/// that run on from the heading.
pub(crate) const SETTINGS_SECTION_GAP: f32 = scale::SP_7;

/// The page's sections, in list order (a muted divider before Archived).
pub(crate) const SETTINGS_SECTIONS: &[(&str, &str)] = &[
    ("general", "General"),
    ("appearance", "Appearance"),
    ("sidebar", "Sidebar"),
    ("notifications", "Notifications"),
    ("browser", "Browser"),
    ("providers", "Providers"),
    ("shortcuts", "Shortcuts"),
    ("archived", "Archived"),
];

/// One-line descriptions per page: the H1's muted line.
pub(crate) fn settings_blurb(section: &str) -> &'static str {
    match section {
        "general" => "Model housekeeping and app defaults. Changes apply live.",
        "appearance" => "How Baaz looks. Changes apply live.",
        "sidebar" => "What the session list shows. Changes apply live.",
        "notifications" => "When Baaz tells you a session needs you. Changes apply live.",
        "browser" => "How Baaz browses: the assistant, tabs, search, downloads.",
        "providers" => "Turn providers on or off and manage their accounts.",
        "shortcuts" => "Click a keycap to rebind it. Changes apply live.",
        "archived" => "Sessions you archived, grouped by project.",
        _ if is_provider_subpage(section) => "Account, tools and defaults for this provider.",
        _ if site_subpage_origin(section).is_some() => "What this site may do, kept from your answers. Changes apply to its next ask.",
        _ => "Settings.",
    }
}

/// `true` for `providers/<wire>` sub-pages (Codex, Claude Code, Muse).
pub(crate) fn is_provider_subpage(section: &str) -> bool {
    provider_subpage_id(section).is_some()
}

/// The provider a sub-page names, or `None`. Pure, so tests drive it
/// without a window.
pub(crate) fn provider_subpage_id(section: &str) -> Option<ProviderId> {
    let wire = section.strip_prefix("providers/")?;
    ProviderId::parse_known(wire)
}

/// Normalize a `settings:<target>` payload to a page id. Handles `""`
/// (last-visited, else General), bare section ids,
/// `providers/<wire>` sub-pages (`settings:providers/codex`), and row ids
/// (`settings:provider-enabled:codex` opens the row's home page, via
/// [`setting_home`]). Unknown ids open the first section. Pure, so steps
/// and tests share it.
pub(crate) fn normalize_settings_target(raw: &str, last_visited: &str) -> String {
    let target = raw.trim().trim_matches('/');
    if target.is_empty() {
        if !last_visited.is_empty() {
            return last_visited.to_owned();
        }
        return "general".to_owned();
    }
    let lower = target.to_lowercase();
    if SETTINGS_SECTIONS.iter().any(|(id, _)| *id == lower) {
        return lower;
    }
    if provider_subpage_id(&lower).is_some() || site_subpage_origin(&lower).is_some() {
        return lower;
    }
    if let Some(home) = setting_home(&lower) {
        // Sub-page rows name their section with a `/*` suffix.
        return home.strip_suffix("/*").unwrap_or(home).to_owned();
    }
    // Legacy dialog ids: the old modal's first section was Sidebar.
    if lower == "sidebar" {
        return "sidebar".to_owned();
    }
    "general".to_owned()
}

#[cfg(feature = "probe")]
/// Whether `settings:<raw>` names something the page can open: empty (the
/// last-visited section), a section id, a provider sub-page, a row id with
/// a home page, or the legacy `sidebar`. Everything
/// [`normalize_settings_target`] would quietly turn into General is
/// unknown, so a scripted step can refuse it.
pub(crate) fn settings_target_known(raw: &str) -> bool {
    let target = raw.trim().trim_matches('/');
    if target.is_empty() {
        return true;
    }
    let lower = target.to_lowercase();
    SETTINGS_SECTIONS.iter().any(|(id, _)| *id == lower)
        || provider_subpage_id(&lower).is_some()
        || site_subpage_origin(&lower).is_some()
        || setting_home(&lower).is_some()
        || lower == "sidebar"
}

/// The section-list selection for a page id: a provider sub-page selects
/// Providers (its sub-pages nest under Providers, indented, only while
/// Providers is selected).
pub(crate) fn settings_parent(section: &str) -> &str {
    if is_provider_subpage(section) {
        "providers"
    } else if site_subpage_origin(section).is_some() {
        "browser"
    } else {
        section
    }
}

/// The header breadcrumb: `Settings / Providers / Codex`.
pub(crate) fn settings_breadcrumb(section: &str) -> Vec<String> {
    let mut crumbs = vec!["Settings".to_owned()];
    if is_provider_subpage(section) {
        crumbs.push("Providers".to_owned());
        if let Some(id) = provider_subpage_id(section) {
            crumbs.push(id.label().to_owned());
        }
        return crumbs;
    }
    if let Some(origin) = site_subpage_origin(section) {
        crumbs.push("Browser".to_owned());
        crumbs.push(crate::browser::history::host_of(origin));
        return crumbs;
    }
    match section {
        "general" => crumbs.push("General".to_owned()),
        "appearance" => crumbs.push("Appearance".to_owned()),
        "sidebar" => crumbs.push("Sidebar".to_owned()),
        "notifications" => crumbs.push("Notifications".to_owned()),
        "browser" => crumbs.push("Browser".to_owned()),
        "providers" => crumbs.push("Providers".to_owned()),
        "shortcuts" => crumbs.push("Shortcuts".to_owned()),
        "archived" => crumbs.push("Archived".to_owned()),
        _ => crumbs.push("General".to_owned()),
    }
    crumbs
}

/// The page crumb `index` of [`settings_breadcrumb`] opens, or `None` for
/// the last crumb (the page already shown): `Settings` opens General and,
/// on a provider sub-page, `Providers` opens the overview.
pub(crate) fn settings_crumb_target(section: &str, index: usize) -> Option<&'static str> {
    let count = settings_breadcrumb(section).len();
    if index + 1 >= count {
        return None;
    }
    match index {
        0 => Some("general"),
        1 if is_provider_subpage(section) => Some("providers"),
        1 if site_subpage_origin(section).is_some() => Some("browser"),
        _ => None,
    }
}

/// Every former setting's exactly-one home page: the row id → page id.
/// Table-driven so the coverage test enumerates it. Both spellings count:
/// the `shortcut:<action>` deep-link id and the rendered row id the page
/// actually carries ([`shortcut_row_id`]); both live on Shortcuts.
pub(crate) fn setting_home(id: &str) -> Option<&'static str> {
    match id {
        "auto_title" | "auto_summary" | "handoff_model_summary" | "fold_finished_turns" | "archive_on_merge" | "open_in" => Some("general"),
        "theme" | "summary_line" => Some("appearance"),
        "group_branch" => Some("sidebar"),
        "notify_needs_you" | "notify_done" | "notify_failed" | "notify_sound" | "notify_badge" => Some("notifications"),
        "search-engine" | "download-dir" => Some("browser"),
        _ if crate::settings_providers::parse_enabled_row(id).is_some() => Some("providers"),
        _ if id.starts_with("use-own-mcp:") || id.starts_with("provider-card:") => Some("providers/*"),
        _ if id.starts_with("shortcut:") || parse_shortcut_row_id(id).is_some() => Some("shortcuts"),
        _ if id.starts_with("archived:") => Some("archived"),
        _ => None,
    }
}

/// The General page's switch rows, from the layout they read: the row
/// model the coverage test enumerates (the page draws its own rows; this
/// table outlived the old modal and only the tests read it now).
#[cfg(test)]
pub(crate) fn general_settings_rows(layout: &Layout) -> Vec<SettingsRow> {
    vec![
        SettingsRow::Switch {
            id: SharedString::from("auto_title"),
            label: SharedString::from("Name sessions automatically"),
            detail: Some(SharedString::from(
                "Spend one cheap call naming a new session after its first message",
            )),
            on: layout.auto_title,
        },
        SettingsRow::Switch {
            id: SharedString::from("auto_summary"),
            label: SharedString::from("Summarise sessions in the sidebar"),
            detail: Some(SharedString::from(
                "Show the last request beside the last reply; rewrite poor ones",
            )),
            on: layout.auto_summary,
        },
        SettingsRow::Switch {
            id: SharedString::from("handoff_model_summary"),
            label: SharedString::from("Summarise handoffs with a model"),
            detail: Some(SharedString::from(
                "A cheap model writes the summary the next provider reads (one short turn). Off: the first lines of the earliest replies.",
            )),
            on: layout.handoff_model_summary,
        },
        SettingsRow::Switch {
            id: SharedString::from("fold_finished_turns"),
            label: SharedString::from("Fold finished turns"),
            detail: Some(SharedString::from(
                "Settled turns fold their work behind one row; the final answer stays out. Off: every run stays expanded.",
            )),
            on: layout.fold_finished_turns,
        },
        SettingsRow::Switch {
            id: SharedString::from("archive_on_merge"),
            label: SharedString::from(ARCHIVE_ON_MERGE_LABEL),
            detail: Some(SharedString::from(ARCHIVE_ON_MERGE_DETAIL)),
            on: layout.archive_on_merge,
        },
    ]
}

/// Settings › General's pull-request switch (docs/82 7.3), shared by the
/// row model and the page.
const ARCHIVE_ON_MERGE_LABEL: &str = "Archive a session when its pull request merges";
const ARCHIVE_ON_MERGE_DETAIL: &str = "Once the pull request of a session's branch reads merged, the session is archived; a session in its own worktree keeps its work. Never while a turn runs.";

/// Settings › Appearance's summary-line switch (docs/83 §4; moved from
/// Sidebar in task 2.9), shared by the row model and the page so the two
/// cannot drift.
const SUMMARY_LINE_LABEL: &str = "Show a summary line";
const SUMMARY_LINE_DETAIL: &str = "Add a second line under each session: what it is waiting on, or what it last said.";

/// The Appearance page's switch rows, from the layout they read: the row
/// model the coverage test enumerates. The theme is a segmented choice,
/// not a switch; its id (`theme`) homes through [`setting_home`].
#[cfg(test)]
pub(crate) fn appearance_settings_rows(layout: &Layout) -> Vec<SettingsRow> {
    vec![SettingsRow::Switch {
        id: SharedString::from("summary_line"),
        label: SharedString::from(SUMMARY_LINE_LABEL),
        detail: Some(SharedString::from(SUMMARY_LINE_DETAIL)),
        on: layout.summary_line,
    }]
}

/// The Sidebar page's switch rows, from the layout they read: the row
/// model the coverage test enumerates (test-only, like the General table).
#[cfg(test)]
pub(crate) fn sidebar_settings_rows(layout: &Layout) -> Vec<SettingsRow> {
    vec![
        SettingsRow::Switch {
            id: SharedString::from("group_branch"),
            label: SharedString::from("Branch name"),
            detail: Some(SharedString::from("Show each project's git branch on its row")),
            on: layout.group_branch,
        },
    ]
}

/// The theme a window opens in: the `--theme` flag when the run names one,
/// else the stored Settings › Appearance choice, System resolved against the
/// system's appearance at launch. A deterministic capture reads no store,
/// so it opens dark unless its entry passes `--theme`.
pub(crate) fn boot_theme(flag: Option<aui_tokens::ThemeKind>, cx: &gpui::App) -> aui_tokens::ThemeKind {
    flag.unwrap_or_else(|| {
        crate::layout::Layout::get(cx).theme.resolve(crate::layout::appearance_is_dark(cx.window_appearance()))
    })
}

/// The stored density and text size, applied once at launch right after
/// `aui::init` (which starts Comfortable / Default). A deterministic capture
/// reads no store, so it stays on the design's values unless a step sets
/// them (`settings-set:density=compact`).
pub(crate) fn boot_type(cx: &mut gpui::App) {
    let (density, text_size) = {
        let layout = crate::layout::Layout::get(cx);
        (layout.density, layout.text_size)
    };
    if density != crate::layout::DensityPref::default() {
        aui_tokens::AuiTheme::set_density(density.density(), None, cx);
    }
    if text_size != crate::layout::TextSizePref::default() {
        aui_tokens::AuiTheme::set_text_size(text_size.text_size(), None, cx);
    }
}

/// The page state. `open` is the route: while open the left column shows
/// the section list and the centre+right merge into the settings surface.
/// Right-pane and terminal state are never written while open — hidden,
/// not closed — so close restores them by doing nothing to them.
#[derive(Clone, Debug, Default)]
pub(crate) struct SettingsPageState {
    /// Whether the page stands open (`Route::Settings`).
    pub open: bool,
    /// The page id: `general` | `appearance` | `sidebar` | `browser` | `providers` |
    /// `providers/<wire>` | `shortcuts` | `archived`.
    pub section: String,
    /// Last-visited section this run (⌘, reopens it; General on first open).
    pub last_visited: String,
    /// The Shortcuts page search field (⌘F focuses it).
    pub shortcut_search: String,
    /// The active session id as it was on open, restored on close.
    pub saved_active: Option<String>,
}

impl SettingsPageState {
    /// Open the page at `target` (`settings:<target>` spelling), saving
    /// the active session for restore. Idempotent: open stays open, it
    /// never toggles.
    pub fn open(&mut self, target: &str, active: Option<String>) {
        let section = normalize_settings_target(target, &self.last_visited.clone());
        if !self.open {
            self.saved_active = active;
        }
        self.open = true;
        self.section = section.clone();
        if !is_provider_subpage(&section) {
            self.last_visited = section;
        } else {
            self.last_visited = "providers".to_owned();
        }
    }

    /// Close and restore: returns the saved active session. Scroll, draft,
    /// focus, right pane and terminal are restored by never having been
    /// touched — the page hides them without writing their state.
    pub fn close(&mut self) -> Option<String> {
        self.open = false;
        self.section.clear();
        self.shortcut_search.clear();
        self.saved_active.take()
    }

    /// Leave for navigation (⌘N, palette jump, session click): the
    /// destination owns the next route, so no restore is needed.
    pub fn exit_for_navigation(&mut self) {
        self.open = false;
        self.section.clear();
        self.shortcut_search.clear();
        self.saved_active = None;
    }
}

/// One Escape press on the page, in priority order (§1.3): recording →
/// search → sub-page up → close+restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettingsEscape {
    /// Cancel the in-flight shortcut recording.
    CancelRecording,
    /// Clear the Shortcuts search field.
    ClearSearch,
    /// A provider sub-page goes up to Providers.
    UpToProviders,
    /// Close the page and restore the session.
    Close,
}

/// Pure Escape order, so tests drive it without a window.
pub(crate) fn settings_escape(
    recording: bool,
    search_nonempty: bool,
    section: &str,
) -> SettingsEscape {
    if recording {
        return SettingsEscape::CancelRecording;
    }
    if search_nonempty {
        return SettingsEscape::ClearSearch;
    }
    if is_provider_subpage(section) {
        return SettingsEscape::UpToProviders;
    }
    SettingsEscape::Close
}

/// The terminal's reserved keys, folded into ONE Reserved row: every
/// `NoAction` ("Terminal takes the key") keystroke in table order.
/// Pure, so tests pin the count.
pub(crate) fn terminal_taken_keys() -> Vec<&'static str> {
    let mut keys = Vec::new();
    for row in crate::keymap::KEYMAP {
        if row.action == "NoAction" && !keys.contains(&row.keystroke) {
            keys.push(row.keystroke);
        }
    }
    keys
}

/// One archived session as the Archived page lists it.
#[derive(Clone, Debug)]
pub(crate) struct ArchivedRow {
    /// The session id Unarchive names.
    pub id: String,
    /// The row's label.
    pub label: String,
    /// The project it is filed under ("Unfiled" without one).
    pub project: String,
    /// When it last moved.
    pub updated: chrono::DateTime<chrono::Local>,
}

/// The Providers page's one-time move row (B4M), from the root's cached
/// plan: the title, and the dry-run's paths behind "Show paths".
#[derive(Clone, Debug)]
pub(crate) struct MigrationRow {
    /// "Move N older …".
    pub title: String,
    /// One line per planned file.
    pub paths: Vec<String>,
}

/// What the page needs from the root: pushed every frame and before every
/// root call into the page (`Harness::with_settings_page`). Only the open
/// section's data is gathered — a closed page gets the defaults. Never
/// persists.
#[derive(Clone, Default)]
pub(crate) struct SettingsFacts {
    /// The provider new sessions start in (the "New sessions" picker).
    pub new_provider: Option<ProviderId>,
    /// Archived: the archived sessions, newest first.
    pub archived: Vec<ArchivedRow>,
    /// Browser: the cached `https` handler (never a live lookup).
    pub default_browser: Option<String>,
    /// Browser: Make default was pressed and macOS has not confirmed.
    pub default_browser_pending: bool,
    /// Browser › Your data: how many pages history holds.
    pub history_pages: u64,
    /// Browser › Your data: the sites history never records.
    pub excluded_sites: Vec<String>,
    /// Browser › Site permissions and a site's page: every site's kept
    /// answers and zoom.
    pub sites: baaz_browser_core::sites::SiteSettings,
    /// Providers: the move row, while moves remain.
    pub migration: Option<MigrationRow>,
    /// A provider sub-page: the import offer under its card.
    pub import_line: Option<crate::import_ui::CardLine>,
}

/// What the page asks of the root: everything that reaches past the
/// page's own state and the layout global.
#[derive(Clone, Debug)]
pub(crate) enum SettingsEvent {
    /// A nav row or crumb: open the page at this target (the root's
    /// `open_settings_page`, which also closes what Settings replaces).
    Navigate(String),
    /// Back, ✕: close and restore.
    Close,
    /// A switch row (or the summary line): flip it and apply it.
    Flip { id: String, on: bool },
    /// Appearance › Theme.
    Theme(crate::layout::ThemePref),
    /// Appearance › Text size.
    TextSize(crate::layout::TextSizePref),
    /// Appearance › Density.
    Density(crate::layout::DensityPref),
    /// A provider's Enable switch.
    FlipProvider { id: ProviderId, on: bool },
    /// A provider card's button (its Details toggle stays on the page).
    ProviderIntent(aui::screens::ProviderIntent),
    /// The "New sessions" picker's pick.
    SetDefaultProvider(ProviderId),
    /// "Set up providers…": the connect screen.
    OpenConnect,
    /// The move row's Move.
    MigrateSessions,
    /// The import line's button.
    OpenImportSheet,
    /// Archived: Unarchive this session.
    Unarchive(String),
    /// Browser › Your data: Clear…
    ClearHistory,
    /// Browser › Your data: Import from Chrome.
    ImportChrome,
    /// Browser › Your data: record this site again.
    IncludeSite(String),
    /// A site's page: keep (`Some`) or drop (`None`) the answer for one
    /// permission kind, by its stored key.
    SetSitePermission { origin: String, key: String, answer: Option<baaz_browser_core::sites::Answer> },
    /// A site's page: back to 100 %.
    ResetSiteZoom(String),
    /// Site permissions › Remove, or a site's page › Forget: everything
    /// kept for the site goes.
    ClearSite(String),
    /// Browser › Default browser: Make default.
    MakeDefaultBrowser,
    /// Browser › Chromium: open `chrome://settings` in Browse.
    OpenChromiumSettings,
}

/// The Settings page (task 4.4): a region entity that owns its route
/// state, its search field and focus, the Shortcuts section's rows and
/// recording, and every menu and fold on its pages; it renders its nav,
/// its crumbs and its surface itself. See the module doc.
pub(crate) struct SettingsPage {
    /// The route: open flag, section, last-visited and restore record.
    pub(crate) state: SettingsPageState,
    /// The Shortcuts search field (⌘F on the page).
    pub(crate) search: Entity<TextareaState>,
    /// General › Worktrees: the branch prefix field. Each edit is stored
    /// (normalised) in the layout as it is typed.
    pub(crate) branch_prefix: Entity<TextareaState>,
    /// The prefix field's edits, stored as they land.
    _branch_prefix_changed: gpui::Subscription,
    /// The surface's focus, tracked on the surface element.
    pub(crate) focus: FocusHandle,
    /// Set when the next render should move the keyboard onto the
    /// surface: armed by every open, so keyboard nav and ⌘F work without a
    /// click first.
    focus_armed: bool,
    /// The Shortcuts section's live rows, re-read on open and after every
    /// edit: a file read per frame is not state.
    pub(crate) shortcuts: Vec<crate::keymap::EffectiveBinding>,
    /// Which Shortcuts row captures the next keystroke, by row id;
    /// exactly one arms at a time.
    pub(crate) recording: Option<String>,
    /// The last refused write per Shortcuts row, by row id: shown as the
    /// row's detail until the next edit on that row.
    pub(crate) shortcut_errors: HashMap<String, String>,
    /// The provider cards whose Details are open (closed by default).
    pub(crate) details_open: HashSet<ProviderId>,
    /// The window's overlay stack: the page's two menus (Browser ›
    /// Assistant's model menu, the "New sessions start in" menu) are
    /// entries on it.
    overlays: Entity<crate::overlays::Overlays>,
    /// Whether the move row shows its dry-run path list (B4M): view
    /// state, never persisted, shut by default.
    pub(crate) migration_paths_expanded: bool,
    /// What the root told the page (see [`SettingsFacts`]).
    pub(crate) facts: SettingsFacts,
    /// The window's provider facts: a provider card reads the Codex
    /// sign-in and what it waits on here, and redraws as they move.
    model: Entity<crate::provider_model::ProviderStatusModel>,
    /// The downloads folder panel: dies with the page.
    tasks: Vec<Task<()>>,
    /// The model observation that redraws an open page.
    _observe: gpui::Subscription,
}

impl EventEmitter<SettingsEvent> for SettingsPage {}

impl SettingsPage {
    /// The page at boot, closed.
    pub(crate) fn new(
        model: Entity<crate::provider_model::ProviderStatusModel>,
        overlays: Entity<crate::overlays::Overlays>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&model, |this, _, cx| {
            if this.state.open {
                cx.notify();
            }
        });
        let prefix = crate::layout::Layout::get(cx).worktree_branch_prefix();
        let branch_prefix = cx.new(|cx| {
            let mut field = aui::composer::composer_state_rows(baaz_core::worktrees::DEFAULT_BRANCH_PREFIX, 1, 1, window, cx);
            field.set_value(prefix, window, cx);
            field
        });
        let branch_prefix_changed = cx.subscribe(&branch_prefix, |_: &mut Self, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let typed = field.read(cx).value().to_string();
                set_worktree_branch_prefix(&typed, cx);
            }
        });
        SettingsPage {
            state: SettingsPageState::default(),
            search: cx.new(|cx| aui::composer::composer_state_rows("Search shortcuts", 1, 1, window, cx)),
            branch_prefix,
            _branch_prefix_changed: branch_prefix_changed,
            focus: cx.focus_handle(),
            focus_armed: false,
            shortcuts: crate::keymap::effective_bindings(),
            recording: None,
            shortcut_errors: HashMap::new(),
            details_open: HashSet::new(),
            overlays,
            migration_paths_expanded: false,
            facts: SettingsFacts::default(),
            model,
            tasks: Vec::new(),
            _observe: observe,
        }
    }

    /// Whether the page stands open (`Route::Settings`).
    pub(crate) fn is_open(&self) -> bool {
        self.state.open
    }

    /// Open at `target` (`settings:<target>` spelling), saving `active`
    /// for close-restore: the Shortcuts rows re-read, a recording
    /// disarmed, the "New sessions" menu shut, the surface's focus armed.
    /// Idempotent: open, never toggle. Only the root calls it, and the
    /// root redraws (the page's view is composed in the root's frame).
    pub(crate) fn open(&mut self, target: &str, active: Option<String>, cx: &mut Context<Self>) {
        self.refresh_shortcuts();
        self.recording = None;
        self.state.open(target, active);
        self.focus_armed = true;
        self.set_default_menu(false, cx);
        cx.notify();
    }

    /// Whether the "New sessions start in" menu stands open (an entry on the
    /// overlay stack).
    pub(crate) fn default_menu_open(&self, cx: &gpui::App) -> bool {
        self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::DefaultProviderMenu)
    }

    /// Open or close the "New sessions start in" menu.
    fn set_default_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.set_menu(crate::overlay_stack::Layer::DefaultProviderMenu, open, cx);
    }

    /// Whether Browser › Assistant's model menu stands open (an entry on the
    /// overlay stack).
    fn assistant_menu_open(&self, cx: &gpui::App) -> bool {
        self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::AssistantMenu)
    }

    /// Open or close one of the page's menus on the overlay stack; Escape
    /// pops it and redraws the page.
    fn set_menu(&mut self, layer: crate::overlay_stack::Layer, open: bool, cx: &mut Context<Self>) {
        let owner = cx.entity_id();
        self.overlays.update(cx, |o, cx| {
            if open {
                o.push(crate::overlay_stack::Entry::new(layer).owner(owner), cx);
            } else {
                o.close(layer.kind(), cx);
            }
        });
    }

    /// Close: the route shuts and a recording disarms. The root restores
    /// the rest and redraws (`Harness::close_settings`).
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.state.close();
        self.recording = None;
        cx.notify();
    }

    /// Leave for navigation: nothing to restore. `false` when the page
    /// was not open; it redraws (and with it the root) when it was.
    pub(crate) fn exit_for_navigation(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.state.open {
            return false;
        }
        self.state.exit_for_navigation();
        self.recording = None;
        cx.notify();
        true
    }

    /// One Escape press on the open page (§1.3 order): what it did. Every
    /// layer but [`SettingsEscape::Close`] is the page's own; closing is
    /// the root's ([`Harness::settings_escape`]).
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> SettingsEscape {
        let step = settings_escape(
            self.recording.is_some(),
            !self.state.shortcut_search.trim().is_empty(),
            &self.state.section,
        );
        match step {
            SettingsEscape::CancelRecording => self.recording = None,
            SettingsEscape::ClearSearch => self.state.shortcut_search.clear(),
            SettingsEscape::UpToProviders => {
                self.state.section = "providers".to_owned();
                self.state.last_visited = "providers".to_owned();
            }
            SettingsEscape::Close => return step,
        }
        cx.notify();
        step
    }

    #[cfg(feature = "probe")]
    /// Open a provider card's Details (the `details:` step verb).
    pub(crate) fn open_details(&mut self, id: ProviderId, cx: &mut Context<Self>) {
        self.details_open.insert(id);
        cx.notify();
    }

    #[cfg(feature = "probe")]
    /// Open the "New sessions start in" menu (the `connect-menu` step verb
    /// while Settings stands open). `false` when the page is shut.
    pub(crate) fn open_default_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.state.open {
            return false;
        }
        self.set_default_menu(true, cx);
        true
    }
}

impl SettingsPage {
    /// The "New sessions" picker's intents: its menu is the page's own;
    /// a pick goes to the root, which remembers it in `provider.json`.
    /// Other intents are not the picker's and do nothing here.
    pub(crate) fn default_provider_intent(&mut self, intent: aui::screens::ConnectIntent, cx: &mut Context<Self>) {
        use aui::screens::ConnectIntent;
        match intent {
            ConnectIntent::ToggleDefaultMenu => {
                let open = !self.default_menu_open(cx);
                self.set_default_menu(open, cx);
            }
            ConnectIntent::CloseDefaultMenu => self.set_default_menu(false, cx),
            ConnectIntent::SetDefault(provider) => {
                self.set_default_menu(false, cx);
                if let Some(id) = crate::providers::provider_from_icon(provider) {
                    cx.emit(SettingsEvent::SetDefaultProvider(id));
                }
            }
            _ => {}
        }
    }

    /// One provider card intent: Details folds on the page; everything else
    /// (switch, re-check, sign in/out, install, docs, path) is the root's.
    fn provider_intent(&mut self, intent: aui::screens::ProviderIntent, cx: &mut Context<Self>) {
        if intent.action == aui::screens::ProviderAction::ToggleDetails {
            let Some(id) = ProviderId::parse_known(intent.id.as_ref()) else { return };
            if !self.details_open.remove(&id) {
                self.details_open.insert(id);
            }
            cx.notify();
            return;
        }
        cx.emit(SettingsEvent::ProviderIntent(intent));
    }
}

impl Focusable for SettingsPage {
    /// The surface: where opening puts the keyboard.
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Region for SettingsPage {
    /// The open page's title ("General", "Codex").
    fn title(&self, _cx: &App) -> SharedString {
        page_title(&self.state.section).into()
    }

    /// The centre header's right end: one ghost close button labelled
    /// "Close settings".
    fn actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let close = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::Close));
        Some(
            icon_button("settings-close", IconName::X)
                .ghost()
                .size(ButtonSize::Sm)
                .accessibility_label("Close settings")
                .on_click(close)
                .into_any_element(),
        )
    }

    /// While open: the page replaces the centre and right columns, so
    /// Browse's page hides under it.
    fn covers_page(&self, _cx: &App) -> bool {
        self.state.open
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The page takes the keyboard the frame after it opens. Skipped in
        // deterministic captures, like the composer's own focus.
        if std::mem::take(&mut self.focus_armed) && self.state.open && !crate::runtime::captures() {
            window.focus(&self.focus, cx);
        }
        self.render_content(window, cx)
    }
}

/// Set the engine an address commit searches with. The Settings →
/// Browser page's rows own it (and `settings-set:search-engine=`); the
/// toolbar reads it on every commit.
pub(crate) fn set_search_engine(engine: SearchEngine, cx: &mut App) {
    crate::layout::Layout::update(cx, |layout| layout.search_engine = engine);
}

/// Point downloads at `dir`: persisted, and passed to the CEF backend at
/// once, like at startup. The Settings → Browser page's Change row and
/// nothing else writes this.
pub(crate) fn set_download_dir(dir: std::path::PathBuf, cx: &mut App) {
    #[cfg(feature = "cef")]
    aui_cef::CefBackend::set_download_dir(dir.clone());
    crate::layout::Layout::update(cx, |layout| layout.download_dir = Some(dir));
}

/// The page title for `section`: a provider sub-page's provider, else the
/// section's label (General for anything unknown).
pub(crate) fn page_title(section: &str) -> &'static str {
    if let Some(id) = provider_subpage_id(section) {
        id.label()
    } else if site_subpage_origin(section).is_some() {
        "Site settings"
    } else {
        SETTINGS_SECTIONS.iter().find(|(id, _)| *id == section).map(|(_, label)| *label).unwrap_or("General")
    }
}

impl SettingsPage {
    /// Re-read the Shortcuts cache, so the section shows the file as it is
    /// now — after a write, or after a hand edit while the page stood open.
    fn refresh_shortcuts(&mut self) {
        self.shortcuts = crate::keymap::effective_bindings();
    }

    /// A Shortcuts row edit, reported through the dialog's `on_shortcut`
    /// intent. `Record` arms the row — exactly one arms at a time; `Set`
    /// rebinds through [`crate::keymap::set_binding`] and reloads the live
    /// bindings, so the new key fires without a restart; `Clear` restores
    /// the default through [`crate::keymap::clear_binding`] and reloads;
    /// `Cancel` disarms. A refused write keeps the old binding and hangs
    /// the error text on the row's detail.
    pub(crate) fn handle_shortcut(&mut self, id: &SharedString, edit: &ShortcutEdit, cx: &mut Context<Self>) {
        match edit {
            ShortcutEdit::Record => {
                if shortcut_is_editable(id) {
                    self.recording = Some(id.to_string());
                    self.shortcut_errors.remove(id.as_ref());
                }
            }
            ShortcutEdit::Set(keystroke) => {
                let accepted = parse_shortcut_row_id(id)
                    .map(|(action, context)| match crate::keymap::set_binding(&action, keystroke, context.as_deref())
                    {
                        Ok(()) => {
                            self.shortcut_errors.remove(id.as_ref());
                            true
                        }
                        Err(error) => {
                            self.shortcut_errors.insert(id.to_string(), error.to_string());
                            false
                        }
                    })
                    .unwrap_or(false);
                self.recording = None;
                if accepted {
                    self.refresh_shortcuts();
                    crate::app::bind_keys(cx);
                }
            }
            ShortcutEdit::Clear => {
                let accepted = parse_shortcut_row_id(id)
                    .map(|(action, context)| match crate::keymap::clear_binding(&action, context.as_deref()) {
                        Ok(()) => {
                            self.shortcut_errors.remove(id.as_ref());
                            true
                        }
                        Err(error) => {
                            self.shortcut_errors.insert(id.to_string(), error.to_string());
                            false
                        }
                    })
                    .unwrap_or(false);
                self.recording = None;
                if accepted {
                    self.refresh_shortcuts();
                    crate::app::bind_keys(cx);
                }
            }
            ShortcutEdit::Cancel => {
                if self.recording.as_deref() == Some(id.as_ref()) {
                    self.recording = None;
                }
            }
        }
        cx.notify();
    }

    /// The Settings → Browser page's Change row: the native folder
    /// panel, directories only — the same panel Add project uses. Cancel
    /// does nothing.
    pub(crate) fn choose_downloads_folder(&mut self, cx: &mut Context<Self>) {
        crate::baaz_log!("choose_downloads_folder: opening the folder panel");
        cx.activate(true);
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            match paths.await {
                Ok(Ok(Some(paths))) => {
                    crate::baaz_log!(
                        "choose_downloads_folder: panel chose {}",
                        paths.first().map(|p| p.display().to_string()).unwrap_or_default()
                    );
                    let _ = this.update(cx, |_, cx| {
                        if let Some(dir) = paths.first() {
                            set_download_dir(dir.clone(), cx);
                        }
                    });
                }
                Ok(Ok(None)) => crate::baaz_log!("choose_downloads_folder: panel cancelled"),
                Ok(Err(error)) => crate::baaz_log!("choose_downloads_folder: panel errored: {error:#}"),
                Err(_) => crate::baaz_log!("choose_downloads_folder: panel future dropped"),
            }
        }));
    }

    /// The Settings nav in the sidebar column: Back, a rule, then one flat
    /// list row per page (provider sub-pages nested under Providers while
    /// it is selected), a rule before Archived.
    pub(crate) fn render_nav(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let selected = settings_parent(&self.state.section);
        let back = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            // A keyboard/VoiceOver press; a pointer press already closed on
            // mouse-down (see below), so this is a no-op then.
            if this.state.open {
                cx.emit(SettingsEvent::Close);
            }
        });
        // Close on mouse-down: closing on click (mouse-up) let the same
        // click land on whatever the closed page uncovered — the
        // Sessions | Browse switch sits exactly under Back, so leaving
        // Settings from Browse flipped to Sessions (found live, B2a).
        let back_down = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
            if this.state.open {
                cx.emit(SettingsEvent::Close);
            }
            cx.stop_propagation();
        });
        let rule = || div().h_px().w_full().flex_none().bg(p.line);
        let list = v_flex()
            .id("settings-nav")
            .size_full()
            .bg(p.sidebar)
            .child(
                div()
                    .debug_selector(|| "settings-back".into())
                    .flex_none()
                    .mx(px(LIST_INSET))
                    // The shell starts this column under its chrome row
                    // and that row's line; Back sits the list inset below
                    // it, as the sidebar's first row does.
                    .mt(px(SETTINGS_NAV_TOP))
                    .mb(px(scale::SP_2))
                    .child(
                        list_row("settings-back", "Back")
                            .leading(aui_icons::icon(IconName::ArrowLeft).size(px(scale::ICON_DEFAULT)))
                            .on_press(back_down)
                            .on_click(back),
                    ),
            )
            .child(rule());
        let mut section_list = v_flex()
            .id("settings-sections")
            .w_full()
            .px(px(LIST_INSET))
            .pt(px(scale::SP_2))
            .gap(px(ROW_GAP))
            .role(gpui::Role::TabList)
            .aria_label("Settings sections");
        for (id, label) in SETTINGS_SECTIONS {
            let target = id.to_string();
            let open = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                cx.emit(SettingsEvent::Navigate(target.clone()));
            });
            section_list = section_list
                .child(list_row(SharedString::from(format!("settings-nav-{id}")), label.to_string()).selected(selected == *id).on_click(open));
            // Provider sub-pages nest under Providers, indented, only
            // while Providers is selected.
            if *id == "providers" && selected == "providers" {
                for provider in ProviderId::all() {
                    let sub = format!("providers/{}", provider.as_str());
                    let sub_active = self.state.section == sub;
                    let open_sub = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                        cx.emit(SettingsEvent::Navigate(sub.clone()));
                    });
                    section_list = section_list.child(
                        list_row(SharedString::from(format!("settings-nav-{}", provider.as_str())), provider.label().to_string())
                            .depth(1)
                            .selected(sub_active)
                            .on_click(open_sub),
                    );
                }
            }
            // A rule before Archived.
            if *id == "shortcuts" {
                section_list = section_list.child(rule().my(px(scale::SP_2)));
            }
        }
        list.child(section_list).into_any_element()
    }

    /// The bar's title while Settings is open: the breadcrumb (`Settings /
    /// Providers / Codex`) as a navigation landmark. The close button is
    /// the page's [`Region::actions`]; the bar has no mode switch.
    pub(crate) fn render_crumbs(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let crumbs = settings_breadcrumb(&self.state.section);
        let mut trail = h_flex().id("settings-breadcrumb").flex_none().items_center().gap(px(scale::SP_2)).role(gpui::Role::Navigation).aria_label(format!(
            "Breadcrumb, {}",
            crumbs.join(" / ")
        ));
        for (index, crumb) in crumbs.iter().enumerate() {
            if index > 0 {
                trail = trail.child(div().flex_none().role_in(TextRole::Body, cx).text_color(p.ink_4).child("/"));
            }
            let last = index + 1 == crumbs.len();
            let crumb_el = div().flex_none().role_in(TextRole::Body, cx).text_color(if last { p.ink } else { p.fg_muted }).child(crumb.clone());
            // Every crumb but the page itself is a link to its page (owner,
            // 2026-10-29: the crumbs looked clickable and were not).
            let crumb_el = match settings_crumb_target(&self.state.section, index) {
                Some(target) => {
                    let open = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                        cx.emit(SettingsEvent::Navigate(target.to_owned()));
                    });
                    interactive(SharedString::from(format!("settings-crumb-{index}")), crumb_el.px(px(scale::SP_1)))
                        .role("link")
                        .label(format!("Open {crumb}"))
                        .on_click(open)
                        .into_any_element()
                }
                None => crumb_el.into_any_element(),
            };
            trail = trail.child(crumb_el);
        }
        trail.into_any_element()
    }

    /// The settings surface: centre+right merged into one column (content
    /// at most [`SETTINGS_CONTENT_MAX`], centred), scrolling on its own,
    /// starting [`SETTINGS_COLUMN_TOP`] under the header's line. The
    /// surface is always the full column, so moving between sections never
    /// resizes anything. Each page: the title (`t-title`) over one Body
    /// `fg-muted` line, then sections [`SETTINGS_SECTION_GAP`] apart (a
    /// `t-subtitle` title over line-split rows, no box around them).
    fn render_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Keep the filter source in one place: the search entity owns the
        // text while the page stands open.
        self.state.shortcut_search =
            self.search.read(cx).value().trim().to_owned();
        let section = self.state.section.clone();
        let p = cx.aui().colors;
        // A site's page is titled by its host (docs/82 8.6).
        let title: String = match site_subpage_origin(&section) {
            Some(origin) => crate::browser::history::host_of(origin),
            None => page_title(&section).to_owned(),
        };
        let blurb = settings_blurb(&section);
        let body: AnyElement = match section.as_str() {
            "general" => self.settings_general_page(window, cx),
            "appearance" => self.settings_appearance_page(cx),
            "sidebar" => self.settings_sidebar_page(cx),
            "notifications" => self.settings_notifications_page(cx),
            "browser" => self.settings_browser_page(cx),
            "providers" => self.settings_providers_overview(window, cx),
            "shortcuts" => self.settings_shortcuts_page(window, cx),
            "archived" => self.settings_archived_page(cx),
            _ if is_provider_subpage(&section) => self.settings_provider_subpage(window, cx),
            _ if site_subpage_origin(&section).is_some() => self.settings_site_page(cx),
            _ => self.settings_general_page(window, cx),
        };
        // While a shortcut records, the next keystroke binds it (the old
        // dialog's capture, moved onto the page): a bare modifier stays
        // armed, anything else sets the row. Escape never reaches here —
        // it matches Cancel first, which disarms through `settings_escape`.
        let capture = self.recording.clone();
        let page = cx.entity().downgrade();
        h_flex()
            .id("settings-surface")
            .size_full()
            .justify_center()
            .track_focus(&self.focus)
            .key_context(SETTINGS_CONTEXT)
            .on_action(cx.listener(|this, _: &SettingsFind, window, cx| {
                window.focus(&this.search.focus_handle(cx), cx);
            }))
            .on_key_down(move |event, _window, cx| {
                let Some(row_id) = &capture else {
                    return;
                };
                if is_bare_modifier(&event.keystroke) {
                    cx.stop_propagation();
                    return;
                }
                let edit = ShortcutEdit::Set(format_keystroke(&event.keystroke));
                let id = SharedString::from(row_id.clone());
                let _ = page.update(cx, |this, cx| this.handle_shortcut(&id, &edit, cx));
                cx.stop_propagation();
            })
            .child(
                v_flex()
                    .id("settings-column")
                    .flex_1()
                    .min_w_0()
                    .max_w(px(SETTINGS_COLUMN_MAX))
                    .h_full()
                    .overflow_y_scroll()
                    // The centre's content starts under its header's line;
                    // the heading sits this far below it.
                    .pt(px(SETTINGS_COLUMN_TOP))
                    .px(px(SETTINGS_COLUMN_PAD_X))
                    .pb(px(SETTINGS_SECTION_GAP))
                    .gap(px(SETTINGS_SECTION_GAP))
                    .child(
                        v_flex()
                            .flex_none()
                            .gap(px(scale::SP_1))
                            .child(
                                div()
                                    .id("settings-h1")
                                    .flex_none()
                                    .role_in(TextRole::Title, cx)
                                    .text_color(p.ink)
                                    .role(gpui::Role::Heading)
                                    .aria_label(format!("Settings page, {title}"))
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .role_in(TextRole::Body, cx)
                                    .text_color(p.fg_muted)
                                    .child(blurb.to_string()),
                            ),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    /// One switch row: the library's settings row, its switch named by the
    /// row's title.
    fn settings_switch_row(
        &self,
        row_id: &str,
        label: &str,
        detail: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row_id.to_owned();
        // The switch asks to flip; the row knows what it showed.
        let flip = cx.listener(move |_: &mut Self, _: &SwitchIntent, _, cx| {
            cx.emit(SettingsEvent::Flip { id: id.clone(), on: !on });
        });
        settings_row(label.to_string())
            .description(detail.to_string())
            .control(
                switch(SharedString::from(format!("settings-switch-{row_id}")), on, false)
                    .accessibility_label(label.to_string())
                    .on_intent(move |intent, window, cx| flip(&intent, window, cx)),
            )
            .into_any_element()
    }

    /// One page section: its title, then rows split by 1 px `line` — no
    /// card around it (docs/83 §7).
    fn settings_group(&self, label: &str, rows: Vec<AnyElement>, _cx: &mut Context<Self>) -> AnyElement {
        section(label.to_string()).rows(rows).into_any_element()
    }

    /// General › Worktrees: the branch prefix a session's worktree branch
    /// starts with (docs/82 6.1), as a one-line field.
    fn settings_worktrees_group(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let field = aui::data::field_frame("settings-branch-prefix", &self.branch_prefix, window, cx)
            .flex_none()
            .w(px(SHORTCUT_SEARCH_W))
            .role(gpui::Role::TextInput)
            .aria_label("Worktree branch prefix")
            .child(
                gpui_kit::component::input::Textarea::new(&self.branch_prefix)
                    .appearance(false)
                    .bordered(false)
                    .role_size(TextRole::Body)
                    .h_auto()
                    .whitespace_nowrap()
                    .into_any_element(),
            );
        let row = div().debug_selector(|| "settings-branch-prefix-row".into()).child(
            settings_row("Branch prefix")
                .description("A session in its own worktree works on a new branch named with this prefix.")
                .control(field),
        );
        self.settings_group("Worktrees", vec![row.into_any_element()], cx)
    }

    /// General: the three model-spending switches under "Model
    /// housekeeping", the transcript's fold, the worktree branch prefix,
    /// and archiving a session when its pull request merges.
    fn settings_general_page(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = vec![
            self.settings_switch_row(
                "auto_title",
                "Name sessions automatically",
                "Each of these spends one short call on the cheapest model. Naming runs after a session's first message.",
                crate::layout::Layout::get(cx).auto_title,
                cx,
            ),
            self.settings_switch_row(
                "auto_summary",
                "Summarise sessions in the sidebar",
                "Show the last request beside the last reply; rewrite poor ones.",
                crate::layout::Layout::get(cx).auto_summary,
                cx,
            ),
            self.settings_switch_row(
                "handoff_model_summary",
                "Summarise handoffs with a model",
                "A cheap model writes the summary the next provider reads (one short turn).",
                crate::layout::Layout::get(cx).handoff_model_summary,
                cx,
            ),
        ];
        let transcript = vec![self.settings_switch_row(
            "fold_finished_turns",
            "Fold finished turns",
            "When a turn ends, its tool calls and in-between notes fold into one \"Worked for …\" row above the answer.",
            crate::layout::Layout::get(cx).fold_finished_turns,
            cx,
        )];
        let pull_requests = vec![self.settings_switch_row(
            "archive_on_merge",
            ARCHIVE_ON_MERGE_LABEL,
            ARCHIVE_ON_MERGE_DETAIL,
            crate::layout::Layout::get(cx).archive_on_merge,
            cx,
        )];
        settings_sections_stack()
            .child(self.settings_group("Model housekeeping", rows, cx))
            .child(self.settings_group("Transcript", transcript, cx))
            .child(self.settings_editor_group(cx))
            .child(self.settings_worktrees_group(window, cx))
            .child(self.settings_group("Pull requests", pull_requests, cx))
            .into_any_element()
    }

    /// Appearance: the theme (task 2.9), text size and density (task 2.10,
    /// through `TextRole` and `Metrics`), and the sidebar's summary line
    /// (moved here from Sidebar: it changes how the list looks, not what
    /// it holds).
    fn settings_appearance_page(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::layout::{DensityPref, TextSizePref, ThemePref};
        let active = ThemePref::ALL.iter().position(|pref| *pref == crate::layout::Layout::get(cx).theme).unwrap_or(0);
        let set = cx.listener(|_: &mut Self, index: &usize, _, cx| {
            if let Some(pref) = ThemePref::ALL.get(*index) {
                cx.emit(SettingsEvent::Theme(*pref));
            }
        });
        let theme = settings_row("Theme")
            .description("System follows macOS as it switches between light and dark.")
            .control(
                div().debug_selector(|| "settings-theme".into()).child(
                    segmented("settings-theme", ThemePref::ALL.iter().map(|pref| SharedString::from(pref.label())).collect(), active)
                        .accessibility_label("Theme")
                        .on_select(move |index, window, cx| set(&index, window, cx)),
                ),
            )
            .into_any_element();
        let size_active = TextSizePref::ALL.iter().position(|pref| *pref == crate::layout::Layout::get(cx).text_size).unwrap_or(0);
        let set_size = cx.listener(|_: &mut Self, index: &usize, _, cx| {
            if let Some(pref) = TextSizePref::ALL.get(*index) {
                cx.emit(SettingsEvent::TextSize(*pref));
            }
        });
        let text_size = settings_row("Text size")
            .description("Large sets every piece of text one step up.")
            .control(
                div().debug_selector(|| "settings-text-size".into()).child(
                    segmented("settings-text-size", TextSizePref::ALL.iter().map(|pref| SharedString::from(pref.label())).collect(), size_active)
                        .accessibility_label("Text size")
                        .on_select(move |index, window, cx| set_size(&index, window, cx)),
                ),
            )
            .into_any_element();
        let density_active = DensityPref::ALL.iter().position(|pref| *pref == crate::layout::Layout::get(cx).density).unwrap_or(0);
        let set_density = cx.listener(|_: &mut Self, index: &usize, _, cx| {
            if let Some(pref) = DensityPref::ALL.get(*index) {
                cx.emit(SettingsEvent::Density(*pref));
            }
        });
        let density = settings_row("Density")
            .description("Compact uses smaller text and shorter rows.")
            .control(
                div().debug_selector(|| "settings-density".into()).child(
                    segmented("settings-density", DensityPref::ALL.iter().map(|pref| SharedString::from(pref.label())).collect(), density_active)
                        .accessibility_label("Density")
                        .on_select(move |index, window, cx| set_density(&index, window, cx)),
                ),
            )
            .into_any_element();
        let summary = self.settings_switch_row(
            "summary_line",
            SUMMARY_LINE_LABEL,
            SUMMARY_LINE_DETAIL,
            crate::layout::Layout::get(cx).summary_line,
            cx,
        );
        settings_sections_stack()
            .child(self.settings_group("Window", vec![theme, text_size, density], cx))
            .child(self.settings_group("Sidebar", vec![summary], cx))
            .into_any_element()
    }



    /// General › Files (docs/82 7.5): "Open files in" — Default app plus
    /// each editor found in `/Applications` and `~/Applications`, as a chip
    /// whose menu lists them. The file preview's open button, a Changes
    /// row's and the session ⋯ menu's "Open project in …" all follow it.
    fn settings_editor_group(&self, cx: &mut Context<Self>) -> AnyElement {
        use aui::composer::{model_menu, PickerRow};
        let p = cx.aui().colors;
        let open = self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::EditorMenu);
        let label = crate::open_in::choice_label(cx);
        let chip = button("settings-open-in", label.clone())
            .trailing(aui_icons::icon(IconName::ChevronDown).size(px(scale::ICON_SM)).color(p.fg_muted))
            .menu_open(open)
            .accessibility_label(format!("Open files in, {label}. Change"))
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                let open = !this.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::EditorMenu);
                this.set_menu(crate::overlay_stack::Layer::EditorMenu, open, cx);
            }));
        let mut anchor = div().relative().flex_none().child(div().debug_selector(|| "settings-open-in".into()).child(chip));
        if open {
            let found = crate::open_in::detected();
            let mut rows = vec![PickerRow::new(baaz_core::editors::DEFAULT_APP, "Default app", "macOS picks by file type")];
            rows.extend(found.iter().map(|f| PickerRow::new(f.editor.id, f.editor.name, f.app.to_string_lossy().into_owned())));
            let selected = crate::open_in::chosen(cx)
                .and_then(|c| found.iter().position(|f| f.editor.id == c.editor.id))
                .map_or(0, |i| i + 1);
            let pick = cx.entity().downgrade();
            let close = pick.clone();
            anchor = anchor.child(
                model_menu("settings-open-in-menu", rows, selected, true)
                    .align_end(true)
                    .drop_down(true)
                    .title("Open files in")
                    .on_pick(move |row, _, cx| {
                        let id = row.to_string();
                        let _ = pick.update(cx, |this, cx| {
                            crate::layout::Layout::stage(cx, |layout| {
                                layout.open_in = (id != baaz_core::editors::DEFAULT_APP).then_some(id);
                            });
                            this.set_menu(crate::overlay_stack::Layer::EditorMenu, false, cx);
                            crate::layout::Layout::save(cx);
                        });
                    })
                    .on_close(move |_, cx| {
                        let _ = close.update(cx, |this, cx| {
                            this.set_menu(crate::overlay_stack::Layer::EditorMenu, false, cx);
                        });
                    }),
            );
        }
        let row = settings_row("Open files in")
            .description("Where a file preview, a Changes row and a session's folder open. An editor that takes a line opens at the selected line.")
            .control(anchor);
        self.settings_group("Files", vec![row.into_any_element()], cx)
    }

    /// The Search group: the four engines as one segmented choice. The
    /// address field commits to the chosen one from the next Enter on.
    fn settings_browser_search_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = SearchEngine::ALL.iter().position(|e| *e == crate::layout::Layout::get(cx).search_engine).unwrap_or(0);
        let set = cx.listener(|_: &mut Self, index: &usize, _, cx| {
            if let Some(engine) = SearchEngine::ALL.get(*index) {
                set_search_engine(*engine, cx);
            }
        });
        let control = div().debug_selector(|| "settings-search-engine".into()).child(
            segmented("settings-search-engine", SearchEngine::ALL.iter().map(|e| SharedString::from(e.name())).collect(), active)
                .accessibility_label("Default search engine")
                .on_select(move |index, w, cx| set(&index, w, cx)),
        );
        self.settings_group(
            "Search",
            vec![settings_row("Default search engine").description("What the address field searches with.").control(control).into_any_element()],
            cx,
        )
    }

    /// The Downloads group: the current folder and its Change row,
    /// which opens the same folder panel Add project uses.
    /// Settings › Browser › Assistant (R3 board 4a/4b): the provider and
    /// model a browser chat starts with, as one chip — the provider's mark,
    /// the model's name, a chevron — whose menu lists every usable
    /// provider's models, the cheapest marked. The default (nothing
    /// stored) is the first connected provider's cheapest model.
    fn settings_browser_assistant_group(&self, cx: &mut Context<Self>) -> AnyElement {
        use aui::composer::{model_menu, PickerRow};
        let p = cx.aui().colors;
        let statuses = crate::provider_status::live_statuses();
        let current = crate::browser::assistant::current_choice(cx);
        let mut anchor = div().relative().flex_none();
        let chip_label = current.as_ref().map(|c| c.label).unwrap_or("No provider connected");
        let mut chip = button("settings-assistant-model", chip_label)
            .trailing(aui_icons::icon(IconName::ChevronDown).size(px(scale::ICON_SM)).color(p.fg_muted))
            .menu_open(self.assistant_menu_open(cx))
            .accessibility_label(format!("Assistant model, {chip_label}. Change"))
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                let open = !this.assistant_menu_open(cx);
                this.set_menu(crate::overlay_stack::Layer::AssistantMenu, open, cx);
            }));
        if let Some(choice) = current.as_ref() {
            chip = chip.leading(aui_icons::provider_mark(choice.provider.icon()).size(px(scale::ICON_DEFAULT)));
        }
        let chip = div().debug_selector(|| "settings-assistant-model".into()).child(chip);
        anchor = anchor.child(chip);
        if self.assistant_menu_open(cx) {
            let choices: Vec<crate::browser::assistant::ModelChoice> = ProviderId::all()
                .into_iter()
                .filter(|id| crate::settings_providers::usable_provider(&statuses, *id))
                .flat_map(crate::browser::assistant::model_choices)
                .collect();
            let rows: Vec<PickerRow> = choices
                .iter()
                .map(|c| {
                    let row = PickerRow::new(format!("{}/{}", c.provider.as_str(), c.model), c.label, c.provider.label()).mark(c.provider.icon());
                    if c.cheapest { row.badge("cheapest") } else { row }
                })
                .collect();
            let selected = current
                .as_ref()
                .and_then(|cur| choices.iter().position(|c| c.provider == cur.provider && c.model == cur.model))
                .unwrap_or(0);
            let pick = cx.entity().downgrade();
            let close = pick.clone();
            anchor = anchor.child(
                model_menu("settings-assistant-menu", rows, selected, true)
                    .align_end(true)
                    .drop_down(true)
                    .title("Browser chats start on")
                    .on_pick(move |row, _, cx| {
                        let (provider, model) = row.split_once('/').unwrap_or((row.as_ref(), ""));
                        let (provider, model) = (provider.to_owned(), model.to_owned());
                        let _ = pick.update(cx, |this, cx| {
                            crate::layout::Layout::stage(cx, |layout| layout.assistant = crate::browser::assistant::AssistantPrefs { provider: Some(provider), model: Some(model) });
                            this.set_menu(crate::overlay_stack::Layer::AssistantMenu, false, cx);
                            crate::layout::Layout::save(cx);
                        });
                    })
                    .on_close(move |_, cx| {
                        let _ = close.update(cx, |this, cx| {
                            this.set_menu(crate::overlay_stack::Layer::AssistantMenu, false, cx);
                        });
                    }),
            );
        }
        let row = settings_row("Provider and model")
            .description("Used by ⌘J, the card and Ask in the Field. New chats start with it; an open chat keeps its model.")
            .control(anchor);
        self.settings_group("Assistant", vec![row.into_any_element()], cx)
    }

    /// Settings › Browser › Agents (docs/27 §6.2, R3 board 4a; owner 8 Nov:
    /// built in R3): Ask or Auto before acting on a page, what a chat may do
    /// with your own tabs, and the per-site rules — each rule one row with
    /// its remove button. "Always on this site" on an approval adds one.
    fn settings_browser_agents_group(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::browser::agents::{ActMode, SiteRule, YourTabs};
        let row = |_id: &'static str, label: &'static str, detail: &'static str, control: AnyElement| -> AnyElement {
            settings_row(label).description(detail).control(control).into_any_element()
        };
        let agents = crate::layout::Layout::get(cx).agents.clone();
        let act_index = match agents.act {
            ActMode::Ask => 0,
            ActMode::Auto => 1,
        };
        let act = segmented("settings-agents-act", vec!["Ask".into(), "Auto".into()], act_index)
            .accessibility_label("Before acting on a page")
            .on_select({
                let set = cx.listener(|_: &mut Self, index: &usize, _, cx| {
                    crate::layout::Layout::update(cx, |layout| layout.agents.act = if *index == 0 { ActMode::Ask } else { ActMode::Auto });
                });
                move |index, w, cx| set(&index, w, cx)
            });
        let tabs_index = match agents.your_tabs {
            YourTabs::Yes => 0,
            YourTabs::AskFirst => 1,
            YourTabs::ReadOnly => 2,
        };
        let tabs = segmented("settings-agents-tabs", vec!["Yes".into(), "Ask first".into(), "Read only".into()], tabs_index)
            .accessibility_label("Your own tabs")
            .on_select({
                let set = cx.listener(|_: &mut Self, index: &usize, _, cx| {
                    let tabs = match *index {
                        0 => YourTabs::Yes,
                        1 => YourTabs::AskFirst,
                        _ => YourTabs::ReadOnly,
                    };
                    crate::layout::Layout::update(cx, |layout| layout.agents.your_tabs = tabs);
                });
                move |index, w, cx| set(&index, w, cx)
            });
        let mut rows = vec![
            row(
                "settings-agents-act-row",
                "Before acting on a page",
                "Auto still asks before Send, Pay, Delete, Upload and signing in.",
                act.into_any_element(),
            ),
            row(
                "settings-agents-tabs-row",
                "Your own tabs",
                "Whether a chat may use tabs you opened, not just its own. \u{201C}This page\u{201D} on the line lends the one you're on.",
                tabs.into_any_element(),
            ),
        ];
        let sites = settings_row("Sites").description(if agents.sites.is_empty() {
            agents.sites_summary()
        } else {
            // The rows below name every rule; the line says what removing
            // one does.
            "Remove a rule to be asked again on that site.".to_owned()
        });
        rows.push(sites.into_any_element());
        for (index, (host, rule)) in agents.sites.iter().enumerate() {
            let word = match rule {
                SiteRule::Always => "Always here",
                SiteRule::Never => "Never here",
            };
            let host_owned = host.clone();
            let remove = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                crate::layout::Layout::update(cx, |layout| layout.agents.set_site(&host_owned, None));
            });
            rows.push(
                site_row(
                    host,
                    Some(word),
                    button(SharedString::from(format!("settings-agents-site-remove-{index}")), "Remove")
                        .accessibility_label(SharedString::from(format!("Remove the rule for {host}")))
                        .on_click(move |e, w, cx| remove(e, w, cx)),
                    cx,
                ),
            );
        }
        self.settings_group("Agents", rows, cx)
    }

    /// R4 "Your data" (board R4Daily 2a): history's size and Clear…, and
    /// the sites history never records, each with Remove. (Import from
    /// Chrome joins this group.)
    fn settings_browser_data_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let pages = self.facts.history_pages;
        let summary = format!(
            "{} · kept {} days · never from an agent’s tabs",
            if pages == 1 { "1 page".to_owned() } else { format!("{pages} pages") },
            crate::browser::history::KEEP_DAYS
        );
        let clear = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::ClearHistory));
        let import = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::ImportChrome));
        let mut rows = vec![
            settings_row("Import from Chrome")
                .description("Bookmarks and history. Chrome is only read; nothing in it changes.")
                .control(
                    button("settings-browser-import-open", "Import…")
                        .accessibility_label("Import bookmarks and history from Chrome")
                        .on_click(move |e, w, cx| import(e, w, cx)),
                )
                .into_any_element(),
            settings_row("History")
                .description(summary)
                .control(
                    button("settings-history-clear", "Clear…")
                        .accessibility_label("Clear browsing history")
                        .on_click(move |e, w, cx| clear(e, w, cx)),
                )
                .into_any_element(),
        ];
        let sites = self.facts.excluded_sites.clone();
        rows.push(
            settings_row("Never remembered")
                .description(if sites.is_empty() {
                    "None. “Never remember this site” in the Field (⌘L) adds the page you’re on.".to_owned()
                } else {
                    "History never records these sites. Remove one to record it again.".to_owned()
                })
                .into_any_element(),
        );
        for (index, site) in sites.into_iter().enumerate() {
            let owned = site.clone();
            let remove = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                cx.emit(SettingsEvent::IncludeSite(owned.clone()));
            });
            rows.push(site_row(
                &site,
                None,
                button(SharedString::from(format!("settings-forgotten-remove-{index}")), "Remove")
                    .accessibility_label(SharedString::from(format!("Record {site} again")))
                    .on_click(move |e, w, cx| remove(e, w, cx)),
                cx,
            ));
        }
        self.settings_group("Your data", rows, cx)
    }

    /// Browser › Site permissions (docs/82 8.6): every site with a kept
    /// answer or zoom, each with Edit (its own page) and Remove.
    fn settings_browser_sites_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let origins = self.facts.sites.origins();
        let mut rows = vec![settings_row("Sites")
            .description(if origins.is_empty() {
                "None yet. \u{201C}Always allow\u{201D} or \u{201C}Always block\u{201D} on a site\u{2019}s ask keeps the answer here.".to_owned()
            } else {
                "Answers kept for good, and pages you zoomed. Remove a site to be asked again.".to_owned()
            })
            .into_any_element()];
        for (index, origin) in origins.iter().enumerate() {
            let host = crate::browser::history::host_of(origin);
            let summary = site_summary(&self.facts.sites, origin);
            let (edit_origin, remove_origin) = (origin.clone(), origin.clone());
            let edit = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                cx.emit(SettingsEvent::Navigate(site_subpage(&edit_origin)));
            });
            let remove = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                cx.emit(SettingsEvent::ClearSite(remove_origin.clone()));
            });
            let actions = h_flex()
                .gap(px(scale::SP_2))
                .child(
                    button(SharedString::from(format!("settings-site-edit-{index}")), "Edit")
                        .accessibility_label(SharedString::from(format!("Edit the settings for {host}")))
                        .on_click(move |e, w, cx| edit(e, w, cx)),
                )
                .child(
                    button(SharedString::from(format!("settings-site-remove-{index}")), "Remove")
                        .accessibility_label(SharedString::from(format!("Remove the settings for {host}")))
                        .on_click(move |e, w, cx| remove(e, w, cx)),
                );
            rows.push(site_row(origin, Some(&summary), actions, cx));
        }
        self.settings_group("Site permissions", rows, cx)
    }

    /// One site's page (docs/82 8.6): each permission kind's kept answer
    /// as Ask / Allow / Block, the agents' rule for the site, its zoom, and
    /// Forget. Opened from Site permissions › Edit, the page menu's "Site
    /// settings…" and the address line's site information.
    fn settings_site_page(&self, cx: &mut Context<Self>) -> AnyElement {
        use baaz_browser_core::sites::Answer;
        use crate::browser::agents::SiteRule;
        let page = self.state.section.clone();
        let Some(origin) = site_subpage_origin(&page).map(str::to_owned) else {
            return div().into_any_element();
        };
        let host = crate::browser::history::host_of(&origin);
        let sites = &self.facts.sites;
        let mut keys: Vec<(String, String)> = SITE_PERMISSION_ROWS.iter().map(|(key, label)| ((*key).to_owned(), (*label).to_owned())).collect();
        for (key, _) in sites.answers(&origin) {
            if !keys.iter().any(|(k, _)| *k == key) {
                keys.push((key, "Another device".to_owned()));
            }
        }
        let mut permissions = Vec::new();
        for (index, (key, label)) in keys.into_iter().enumerate() {
            let current = match sites.answer(&origin, &key) {
                None => 0,
                Some(Answer::Allow) => 1,
                Some(Answer::Block) => 2,
            };
            let (origin_owned, key_owned) = (origin.clone(), key.clone());
            let set = cx.listener(move |_: &mut Self, index: &usize, _, cx| {
                let answer = match *index {
                    1 => Some(Answer::Allow),
                    2 => Some(Answer::Block),
                    _ => None,
                };
                cx.emit(SettingsEvent::SetSitePermission { origin: origin_owned.clone(), key: key_owned.clone(), answer });
            });
            let control = segmented(SharedString::from(format!("settings-site-permission-{index}")), vec!["Ask".into(), "Allow".into(), "Block".into()], current)
                .accessibility_label(SharedString::from(format!("{label} on {host}")))
                .on_select(move |index, w, cx| set(&index, w, cx));
            permissions.push(settings_row(label).control(control).into_any_element());
        }
        // The agents' rule is the host's (Settings › Browser › Agents › Sites).
        let rule = crate::layout::Layout::get(cx).agents.site(&host);
        let rule_index = match rule {
            None => 0,
            Some(SiteRule::Always) => 1,
            Some(SiteRule::Never) => 2,
        };
        let rule_host = host.clone();
        let set_rule = cx.listener(move |_: &mut Self, index: &usize, _, cx| {
            let rule = match *index {
                1 => Some(SiteRule::Always),
                2 => Some(SiteRule::Never),
                _ => None,
            };
            crate::layout::Layout::update(cx, |layout| layout.agents.set_site(&rule_host, rule));
        });
        let agents = settings_row("Agents on this site")
            .description(match rule {
                None => "As Settings \u{203A} Browser \u{203A} Agents says.",
                Some(SiteRule::Always) => "Agents act here without asking.",
                Some(SiteRule::Never) => "Agents may not act here.",
            })
            .control(
                segmented("settings-site-agents", vec!["Default".into(), "Always".into(), "Never".into()], rule_index)
                    .accessibility_label(SharedString::from(format!("Agents on {host}")))
                    .on_select(move |index, w, cx| set_rule(&index, w, cx)),
            )
            .into_any_element();
        let zoom = sites.zoom(&origin);
        let mut zoom_row = settings_row("Zoom").description(match zoom {
            Some(zoom) => format!("{zoom}% on every page of this site"),
            None => "100% (pages here open at the normal size)".to_owned(),
        });
        if zoom.is_some() {
            let reset_origin = origin.clone();
            let reset = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::ResetSiteZoom(reset_origin.clone())));
            zoom_row = zoom_row.control(
                button("settings-site-zoom-reset", "Reset")
                    .accessibility_label(SharedString::from(format!("Reset the zoom for {host}")))
                    .on_click(move |e, w, cx| reset(e, w, cx)),
            );
        }
        let forget_origin = origin.clone();
        let forget = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::ClearSite(forget_origin.clone())));
        let forget_row = settings_row("Forget this site\u{2019}s settings")
            .description("Every answer above goes back to Ask and the zoom to 100%.")
            .control(
                button("settings-site-forget", "Forget")
                    .danger()
                    .accessibility_label(SharedString::from(format!("Forget the settings for {host}")))
                    .on_click(move |e, w, cx| forget(e, w, cx)),
            )
            .into_any_element();
        settings_sections_stack()
            .child(
                section(origin.clone())
                    .rows(permissions)
                    .note("Ask shows the strip above the page the next time the site asks."),
            )
            .child(self.settings_group("Agents", vec![agents], cx))
            .child(self.settings_group("Page", vec![zoom_row.into_any_element(), forget_row], cx))
            .into_any_element()
    }

    /// R4 Ad blocking: the switch and the sites ads are allowed on.
    fn settings_browser_adblock_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = crate::layout::Layout::get(cx).adblock.clone();
        let mut rows = vec![self.settings_switch_row(
            "adblock_enabled",
            "Block ads and trackers",
            "EasyList and EasyPrivacy, refreshed weekly. Pages load without them; nothing you browse is sent anywhere.",
            prefs.enabled,
            cx,
        )];
        rows.push(
            settings_row("Allowed sites")
                .description(if prefs.allowed.is_empty() {
                    "Ads are blocked on every site. “Allow ads on this site” in the Field (⌘L) makes an exception.".to_owned()
                } else {
                    "Ads are allowed on these sites:".to_owned()
                })
                .into_any_element(),
        );
        for (index, site) in prefs.allowed.iter().enumerate() {
            let owned = site.clone();
            let remove = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                crate::layout::Layout::update(cx, |layout| {
                    layout.adblock.toggle(&owned);
                });
                crate::browser::adblock::apply(&crate::layout::Layout::get(cx).adblock);
            });
            rows.push(site_row(
                site,
                None,
                button(SharedString::from(format!("settings-adblock-remove-{index}")), "Remove")
                    .accessibility_label(SharedString::from(format!("Block ads on {site} again")))
                    .on_click(move |e, w, cx| remove(e, w, cx)),
                cx,
            ));
        }
        self.settings_group("Ad blocking", rows, cx)
    }

    fn settings_browser_downloads_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let dir = browser_download_dir(crate::layout::Layout::get(cx));
        let row = div().debug_selector(|| "settings-browser-downloads".into()).child(
            settings_row("Downloads folder").description(tilde_path(&dir)).control(
                div().debug_selector(|| "settings-downloads-change".into()).child(
                    button("settings-downloads-change", "Change…")
                        .accessibility_label("Change downloads folder")
                        .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            this.choose_downloads_folder(cx);
                        })),
                ),
            ),
        );
        self.settings_group("Downloads", vec![row.into_any_element()], cx)
    }

    /// The Chromium row: everything Chromium owns — passwords, cookies,
    /// site settings — lives at `chrome://settings`, opened in a Browse
    /// tab.
    fn settings_browser_chromium_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| {
            cx.emit(SettingsEvent::OpenChromiumSettings);
        });
        let row = div().debug_selector(|| "settings-browser-chromium".into()).child(
            settings_row("Chromium settings")
                .description("Passwords, cookies, site settings and everything else Chromium owns.")
                .control(
                    button("settings-browser-chromium", "Open chrome://settings")
                        .trailing(aui_icons::icon(IconName::External).size(px(scale::ICON_SM)))
                        .accessibility_label("Open Chromium settings")
                        .on_click(open),
                ),
        );
        self.settings_group("Chromium", vec![row.into_any_element()], cx)
    }

    /// Browser (B2a-H6): tabs, search, downloads, the Chromium row — and
    /// the Default-browser row below, which B2a-H7 owns. The
    /// Default-browser row renders the cached `https` handler only — the
    /// Browse view owns the LaunchServices lookup — and Make default
    /// claims `http` and `https` for Baaz (macOS shows its own
    /// confirmation and applies it asynchronously, so the row waits until
    /// the next re-read). The button is the only place that ever changes
    /// the system default. Under `BAAZ_DETERMINISTIC=1` the row wears its
    /// fixed capture text instead, so captures never depend on the
    /// machine's handler.
    fn settings_browser_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let handler = self.facts.default_browser.clone();
        let status = crate::default_browser::row_status_text(
            handler.as_deref(),
            self.facts.default_browser_pending,
        );
        // B2a-H7
        let mut row = settings_row("Default browser").description(status);
        if crate::default_browser::offers_make_default(handler.as_deref()) {
            let make = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(SettingsEvent::MakeDefaultBrowser));
            row = row.control(
                button("settings-browser-make-default", "Make default")
                    .accessibility_label("Make Baaz your default browser")
                    .on_click(move |_, window, cx| make(&(), window, cx)),
            );
        }
        // Chromium owns its settings UI, so the row shows only where the
        // engine can load `chrome://` pages (the `cef` backend) — anywhere
        // else it would be a dead row.
        let mut page = settings_sections_stack()
            .child(self.settings_browser_data_group(cx))
            .child(self.settings_browser_sites_group(cx))
            .child(self.settings_browser_assistant_group(cx))
            .child(self.settings_browser_agents_group(cx))
            .child(self.settings_browser_search_group(cx))
            .child(self.settings_browser_adblock_group(cx))
            .child(self.settings_browser_downloads_group(cx));
        if cfg!(feature = "cef") {
            page = page.child(self.settings_browser_chromium_group(cx));
        }
        page.child(
            section("Default browser")
                .row(row)
                .note("Links from other apps open as person tabs in Browse."),
        )
        .into_any_element()
    }

    /// Sidebar: the branch name — plus the note that grouping and
    /// sort live in the sidebar's view menu (the summary line lives on
    /// Appearance). The active
    /// session's project marks itself with an accent bar in every mode; no
    /// switch gates it anymore.
    fn settings_sidebar_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = vec![
            self.settings_switch_row(
                "group_branch",
                "Branch name",
                "Show each project's git branch on its row.",
                crate::layout::Layout::get(cx).group_branch,
                cx,
            ),
        ];
        settings_sections_stack()
            .child(
                section("Project rows")
                    .rows(rows)
                    .note("Grouping and sort live in the sidebar's view menu."),
            )
            .into_any_element()
    }

    /// Notifications (task 3.3, 3.5): which moves post a Mac notification,
    /// the sound, and the Dock badge. macOS asks permission the first time
    /// one fires; the note says so, since the switches cannot.
    fn settings_notifications_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = crate::layout::Layout::get(cx).notify_prefs();
        let events = vec![
            self.settings_switch_row("notify_needs_you", "Needs you", "A session waits for your approval or answer.", prefs.needs_you, cx),
            self.settings_switch_row("notify_done", "Finished", "A turn finished while you were in another session or app.", prefs.done, cx),
            self.settings_switch_row("notify_failed", "Failed", "A turn stopped with an error.", prefs.failed, cx),
        ];
        let extras = vec![
            self.settings_switch_row("notify_sound", "Sound", "Play a short sound with each notification.", prefs.sound, cx),
            self.settings_switch_row("notify_badge", "Dock badge", "Count the sessions that need you on the Dock icon.", prefs.badge, cx),
        ];
        settings_sections_stack()
            .child(
                section("Notify me when")
                    .rows(events)
                    .note("Nothing is sent for the session you are looking at. macOS asks once, when Baaz starts, whether to allow notifications."),
            )
            .child(section("Also").rows(extras))
            .into_any_element()
    }

    /// Providers overview: one row per provider in the one provider order
    /// (provider mark, name, headline, Enable switch, chevron into the
    /// sub-page) plus the footer action row. The chevron is decorative —
    /// the text beside it carries the click and the label — but always
    /// rendered, so every row reads as a drill-in (§1.3).
    fn settings_providers_overview(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let statuses = crate::provider_status::live_statuses();
        let mut rows = Vec::new();
        for status in &statuses {
            let wire = status.provider.as_str().to_owned();
            let open = cx.listener({
                let wire = wire.clone();
                move |_: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    cx.emit(SettingsEvent::Navigate(format!("providers/{wire}")));
                }
            });
            let provider = status.provider;
            let enabled = status.enabled;
            let flip = cx.listener(move |_: &mut Self, _: &SwitchIntent, _, cx| {
                cx.emit(SettingsEvent::FlipProvider { id: provider, on: !enabled });
            });
            let name = status.provider.label();
            // The switch, then the drill-in into the provider's page.
            let control = h_flex()
                .gap(px(scale::SP_3))
                .child(
                    switch(SharedString::from(format!("settings-enable-{wire}")), enabled, false)
                        .accessibility_label(SharedString::from(format!("Enable {name}")))
                        .on_intent(move |intent, window, cx| flip(&intent, window, cx)),
                )
                .child(
                    icon_button(SharedString::from(format!("settings-provider-open-{wire}")), IconName::Chevron)
                        .ghost()
                        .accessibility_label(format!("{name} settings, {}. Open the {name} page.", status.headline_text()))
                        .on_click(open),
                );
            rows.push(settings_row(name).description(status.headline_text().to_string()).control(control).into_any_element());
        }
        let setup = cx.listener(|_: &mut Self, _: &(), _, cx| cx.emit(SettingsEvent::OpenConnect));
        let _ = window;
        settings_sections_stack()
            .child(self.settings_group("Providers", rows, cx))
            .child(self.settings_default_provider_group(&statuses, cx))
            // The one-time move of Baaz's older sessions into its own
            // homes, while any remain (B4M).
            .children(self.migration_row(cx))
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap(px(scale::SP_3))
                    .child(
                        div()
                            .flex_1()
                            .role_in(TextRole::Caption, cx)
                            .text_color(p.fg_muted)
                            .child("Signing a CLI out signs it out on this Mac.".to_string()),
                    )
                    .child(
                        button("settings-providers-setup", "Set up providers…")
                            .accessibility_label("Set up providers")
                            .on_click(move |_, window, cx| setup(&(), window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// "New sessions": the connect footer's own picker — "New sessions
    /// start in [mark Name ⌄]", Connected providers only, the current one
    /// checked — writing the same default (`provider.json`). With nothing
    /// Connected the row says so instead. Under the Providers group, so the
    /// menu, which hangs above its chip, opens over the rows and never
    /// under the page's top edge.
    fn settings_default_provider_group(
        &self,
        statuses: &[crate::provider_status::ProviderStatus],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // With no pick, the first in the one order stands (as at boot).
        let current = self.facts.new_provider.unwrap_or_else(ProviderId::first);
        let row: AnyElement = match crate::connect::default_picker_for(statuses, current, self.default_menu_open(cx)) {
            Some(picker) => {
                let weak = cx.entity().downgrade();
                let control = aui::screens::default_provider_picker(
                    "settings-default-provider",
                    picker,
                    move |intent, _window, cx| {
                        let _ = weak.update(cx, |this, cx| this.default_provider_intent(intent, cx));
                    },
                    cx,
                );
                h_flex()
                    .id("settings-default-provider-row")
                    .w_full()
                    .py(px(SETTINGS_ROW_PAD_Y))
                    .role(gpui::Role::Group)
                    .aria_label("Default provider for new sessions")
                    .child(control)
                    .into_any_element()
            }
            None => settings_row("No provider is connected yet")
                .description("New sessions start in the first one you connect.")
                .into_any_element(),
        };
        self.settings_group("New sessions", vec![row], cx)
    }

    /// One provider sub-page: the Account group (today's card) plus the
    /// Tools group ("Use my own MCP servers", Claude Code and Codex only).
    fn settings_provider_subpage(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(id) = provider_subpage_id(&self.state.section) else {
            return self.settings_providers_overview(window, cx);
        };
        let statuses = crate::provider_status::live_statuses();
        let status = statuses.iter().find(|s| s.provider == id);
        let Some(status) = status else {
            return self.settings_providers_overview(window, cx);
        };
        let model = self.model.read(cx);
        let data = crate::settings_providers::with_busy(
            crate::settings_providers::card_data(status, &model.codex_login, self.details_open.contains(&id)),
            model.busy.get(&id).copied(),
        );
        let intent = cx.listener(move |this: &mut Self, intent: &aui::screens::ProviderIntent, _, cx| {
            this.provider_intent(intent.clone(), cx);
        });
        let assist = cx.listener(|this: &mut Self, key: &gpui::SharedString, _, cx| {
            this.model.update(cx, |model, cx| model.codex_assist_link(key, cx));
        });
        // The card sits on the page itself (the D5 board), not inside a
        // settings group; the MCP switch keeps its own Tools group under it.
        let card = aui::screens::provider_card(format!("settings-card-{}", id.as_str()), &data)
            .on_intent(move |event, window, cx| intent(&event, window, cx))
            .on_assist(move |key, window, cx| assist(&key, window, cx))
            .into_any_element();
        // The P7 offer sits under Details, inside the card's frame (the
        // board). The provider card has no footer slot, so the two share
        // one flat card here, a 1 px line between them.
        let card = match self.facts.import_line.as_ref() {
            Some(line) => {
                let open = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(SettingsEvent::OpenImportSheet));
                let line = crate::import_ui::card_line_row(id, line, open, cx);
                aui::data::card().flush().label(format!("{} account", id.label())).section(card).section(line).into_any_element()
            }
            None => card,
        };
        let mut page = settings_sections_stack().child(card);
        if crate::settings_providers::own_mcp_providers().contains(&id) {
            page = page.child(self.settings_group("Tools", vec![self.use_own_mcp_row(id, cx)], cx));
        }
        page.into_any_element()
    }

    /// Shortcuts: a search field pinned at the top (⌘F focuses it),
    /// groups by category, non-editable rows folded into one final
    /// Reserved row, and Restore defaults only when something differs.
    fn settings_shortcuts_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let query = self.search.read(cx).value().trim().to_lowercase();
        let bindings = &self.shortcuts;
        let differs = shortcuts_differ_from_defaults(bindings);
        let restore = cx.listener(|this: &mut Self, _: &(), _, cx| {
            crate::keymap::restore_defaults();
            this.refresh_shortcuts();
            crate::app::bind_keys(cx);
            cx.notify();
        });
        let mut column = settings_sections_stack();
        let mut header_row = h_flex().w_full().flex_none().items_center().gap(px(scale::SP_3));
        header_row = header_row.child(
            aui::data::field_frame("settings-shortcut-search", &self.search, window, cx)
                .flex_none()
                .w(px(SHORTCUT_SEARCH_W))
                .role(gpui::Role::SearchInput)
                .aria_label("Search shortcuts")
                .child(
                    gpui_kit::component::input::Textarea::new(&self.search)
                        .appearance(false)
                        .bordered(false)
                        .role_size(TextRole::Body)
                        .h_auto()
                        .whitespace_nowrap()
                        .into_any_element(),
                ),
        );
        if differs {
            header_row = header_row.child(div().flex_1()).child(
                button("settings-shortcuts-restore", "Restore defaults")
                    .accessibility_label("Restore default shortcuts")
                    .on_click(move |_, window, cx| restore(&(), window, cx)),
            );
        }
        column = column.child(header_row);
        // Group live rows by relabelled category, filtered by the query.
        let mut groups: Vec<(String, Vec<AnyElement>)> = Vec::new();
        for binding in bindings.iter() {
            if !query.is_empty()
                && !binding.label.to_lowercase().contains(&query)
                && !binding.keystroke.to_lowercase().contains(&query)
            {
                continue;
            }
            let id = shortcut_row_id(&binding.action, binding.context.as_deref());
            let recording = self.recording.as_deref() == Some(id.as_str());
            let detail = if let Some(error) = self.shortcut_errors.get(&id) {
                Some(error.clone())
            } else if !binding.editable {
                binding.reserved_reason.clone()
            } else {
                binding.context.as_deref().and_then(context_label).map(str::to_owned)
            };
            let row_id = SharedString::from(id.clone());
            let arm = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.handle_shortcut(&row_id, &ShortcutEdit::Record, cx);
            });
            let editable = binding.editable;
            let keycap = keystroke_glyphs(&binding.keystroke);
            let keycap_button = button(SharedString::from(format!("settings-key-{id}")), if recording { "press a key…".to_owned() } else { keycap.clone() })
                .size(ButtonSize::Sm)
                .toggled(recording)
                .accessibility_label(format!(
                    "{}, currently {}. Activate to rebind.",
                    binding.label,
                    if recording { "recording, press a key" } else { keycap.as_str() }
                ))
                .on_click(arm);
            let row = settings_row(binding.label.clone());
            let row = match detail {
                Some(text) => row.description(text),
                None => row,
            }
            .control(keycap_button)
            .into_any_element();
            let _ = editable;
            let group = shortcut_group_label(&binding.category);
            match groups.iter_mut().find(|(name, _)| *name == group) {
                Some((_, rows)) => rows.push(row),
                None => groups.push((group.to_string(), vec![row])),
            }
        }
        for (name, rows) in groups {
            column = column.child(self.settings_group(&name, rows, cx));
        }
        // The Reserved group, listed ONCE: the terminal's keys in one row.
        let taken = terminal_taken_keys();
        if !taken.is_empty() {
            let shown: Vec<String> = taken.iter().map(|key| keystroke_glyphs(key)).collect();
            let reserved_row = settings_row("Terminal takes the key")
                .description("These keys reach the running program while the terminal is focused.")
                .control(
                    div()
                        .id("settings-reserved-keys")
                        .role_in(TextRole::Caption, cx)
                        .text_color(p.ink)
                        .role(gpui::Role::Label)
                        .aria_label(format!("Reserved terminal keys, {}", shown.join(", ")))
                        .child(shown.join(" · ")),
                )
                .into_any_element();
            column = column.child(self.settings_group("Reserved", vec![reserved_row], cx));
        }
        let _ = window;
        column.into_any_element()
    }

    /// Archived: archived sessions grouped by project, each with
    /// Unarchive. Empty state: one muted line.
    fn settings_archived_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let archived: Vec<&ArchivedRow> = self.facts.archived.iter().collect();
        if archived.is_empty() {
            return v_flex()
                .w_full()
                .child(
                    div()
                        .id("settings-archived-empty")
                        .w_full()
                        .role_in(TextRole::Body, cx)
                        .text_color(p.fg_muted)
                        .role(gpui::Role::Label)
                        .aria_label("No archived sessions")
                        .child("No archived sessions.".to_string()),
                )
                .into_any_element();
        }
        let mut order: Vec<String> = Vec::new();
        let mut by_project: std::collections::HashMap<String, Vec<&ArchivedRow>> =
            std::collections::HashMap::new();
        for entry in archived {
            let name = entry.project.clone();
            by_project.entry(name.clone()).or_default().push(entry);
            if !order.contains(&name) {
                order.push(name);
            }
        }
        let mut column = settings_sections_stack();
        for name in order {
            let rows = by_project.remove(&name).unwrap_or_default();
            let mut els = Vec::new();
            for entry in rows {
                let id = entry.id.clone();
                let unarchive = cx.listener(move |_: &mut Self, _: &(), _, cx| {
                    cx.emit(SettingsEvent::Unarchive(id.clone()));
                });
                els.push(
                    settings_row(entry.label.clone())
                        .description(entry.updated.format("%Y-%m-%d").to_string())
                        .control(
                            button(SharedString::from(format!("settings-unarchive-{}", entry.id)), "Unarchive")
                                .accessibility_label(format!("Unarchive {}", entry.label))
                                .on_click(move |_, window, cx| unarchive(&(), window, cx)),
                        )
                        .into_any_element(),
                );
            }
            column = column.child(self.settings_group(&name, els, cx));
        }
        column.into_any_element()
    }
}

// ── the root's side ─────────────────────────────────────────────────────

impl Harness {

    /// The active session id for the page's restore record, if any.
    fn settings_active_id(&self, cx: &mut Context<Self>) -> Option<String> {
        self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone())
    }

    /// Open the Settings page on the Providers section: where the account
    /// menu's "Providers…" row lands. Idempotent (open, never toggle).
    pub(crate) fn open_providers(&mut self, cx: &mut Context<Self>) {
        self.open_settings_page("providers", cx);
    }

    /// Open the Settings page on legacy dialog index `section` (menu rows,
    /// ⌘,): an index into [`SETTINGS_SECTIONS`] (0 General, 1 Appearance,
    /// 2 Sidebar, 3 Notifications, 4 Browser, 5 Providers, 6 Shortcuts,
    /// 7 Archived). Idempotent (open, never toggle). Menus, palette and
    /// modal go first.
    pub(crate) fn open_settings(&mut self, section: usize, cx: &mut Context<Self>) {
        let target = SETTINGS_SECTIONS.get(section).map(|(id, _)| *id).unwrap_or("general");
        self.open_settings_page(target, cx);
    }

    /// Open the Settings page at `target` (`settings:<target>` spelling).
    /// Saves the active session for close-restore; hides (never writes)
    /// right-pane and terminal state. Opening closes the Skills page, so
    /// only one full-page route stands open. Opening arms the surface's
    /// focus, so keyboard nav and ⌘F work without a click first.
    pub(crate) fn open_settings_page(&mut self, target: &str, cx: &mut Context<Self>) {
        let active = self.settings_active_id(cx);
        self.settings_page.update(cx, |page, cx| page.open(target, active, cx));
        // Settings replaces the Skills page as it does the transcript.
        self.leave_skills(cx);
        self.overlays.update(cx, |overlays, cx| {
            overlays.close_menu(cx);
            overlays.close_palette(cx);
            overlays.close_dialog(cx);
        });
        // Settings → Providers recomputes the B4M plan off the UI thread:
        // the row renders the cache (never the walker) and follows when
        // the recompute lands.
        if target.split('/').next() == Some(crate::settings_providers::PROVIDERS_SECTION_ID) {
            self.refresh_migration_cache(cx);
            // The import offer on the Claude Code / Codex cards (docs/72):
            // the cached plan renders at once; a stale one recomputes
            // behind it, never on the render path.
            self.refresh_import_scan_if_stale(cx);
        }
    }

    /// Close the Settings page, restoring the saved session. Scroll,
    /// draft, focus, right pane and terminal come back exactly: the page
    /// never wrote their state, it only hid them. Closing hands the
    /// keyboard to the composer next frame.
    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_page.update(cx, |page, cx| page.close(cx));
        self.arm_composer_focus(cx);
    }

    /// One Escape press on the page (§1.3 order): cancel a recording, clear
    /// the Shortcuts search, go up from a provider sub-page — the page's own
    /// layers ([`SettingsPage::escape`]) — else close and restore. Returns
    /// `true` when the page stood open.
    pub(crate) fn settings_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.settings_open(cx) {
            return false;
        }
        match self.settings_page.update(cx, |page, cx| page.escape(cx)) {
            SettingsEscape::Close => self.close_settings(cx),
            // Up from a sub-page moves the nav's selection and the crumbs,
            // which the root's frame draws: the page's own notify redraws it.
            SettingsEscape::UpToProviders | SettingsEscape::CancelRecording | SettingsEscape::ClearSearch => {}
        }
        true
    }

    /// Leave the page for navigation (⌘N, palette jump, session click):
    /// the destination owns the next route, so nothing is restored.
    pub(crate) fn exit_settings_for_navigation(&mut self, cx: &mut Context<Self>) {
        self.settings_page.update(cx, |page, cx| page.exit_for_navigation(cx));
    }

    #[cfg(feature = "probe")]
    /// `settings[:<section>]`: open the page, optionally at that section
    /// (`settings:sidebar`, `settings:providers/codex`, or a row id that
    /// names its home page). Empty reopens the last-visited section
    /// (General on first open). A target the page does not know is an
    /// `Err` and opens nothing — the page would otherwise fall back to
    /// General and the capture would show the wrong section. Idempotent:
    /// open, never toggle.
    pub(crate) fn step_settings(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let target = rest.trim();
        if !settings_target_known(target) {
            return Err(format!("no settings section or row named {target:?}"));
        }
        self.open_settings_page(target, cx);
        Ok(())
    }

    /// Flip one Settings switch by its row id and persist it.
    ///
    /// Unknown ids are ignored: a later section adds its own id to
    /// [`apply_setting`].
    pub(crate) fn flip_setting(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if let Some(provider) = crate::settings_providers::parse_enabled_row(id) {
            self.flip_provider_enabled(provider, on, cx);
            return;
        }
        // Every reader observes the layout global, so the change reaches
        // them all from here — the fold switch re-folds every open view,
        // parked ones included (B12fix), with no push into them.
        if !crate::layout::Layout::stage(cx, |layout| apply_setting(layout, id, on)) {
            return;
        }
        crate::layout::Layout::save(cx);
        self.invalidate_list(cx);
        // A Notifications switch applies now: the Dock badge follows its
        // switch without waiting for a session to move (task 3.5).
        if id.starts_with("notify_") {
            self.check_attention(cx);
        }
    }

    /// Settings › Appearance › Theme: store `pref` and paint with it now.
    /// The stored choice is what the next launch opens in (see
    /// [`boot_theme`]); View → Toggle Theme goes through here too, so the
    /// page always shows what the window paints.
    pub(crate) fn set_theme_pref(
        &mut self,
        pref: crate::layout::ThemePref,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        crate::layout::Layout::update(cx, |layout| layout.theme = pref);
        self.apply_theme_pref(window, cx);
    }

    /// Paint with the stored theme choice, System resolved against the
    /// system's appearance now. A no-op when the window already paints it.
    /// The window's appearance observer calls this while the choice is
    /// System, so a macOS switch repaints Baaz too.
    pub(crate) fn apply_theme_pref(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let kind = crate::layout::Layout::get(cx).theme.resolve(crate::layout::appearance_is_dark(cx.window_appearance()));
        if cx.aui().kind != kind {
            aui_tokens::AuiTheme::set_kind(kind, window, cx);
            // Rows cached with the old palette rebuild on the next frame.
            self.invalidate_list(cx);
        }
        cx.notify();
    }

    /// Settings › Appearance › Density: store `pref` and lay the window out
    /// at it now — every text role and the shared row height (task 2.10).
    pub(crate) fn set_density_pref(
        &mut self,
        pref: crate::layout::DensityPref,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        crate::layout::Layout::update(cx, |layout| layout.density = pref);
        if cx.aui().density != pref.density() {
            aui_tokens::AuiTheme::set_density(pref.density(), window, cx);
            // Rows cached at the old sizes rebuild on the next frame.
            self.invalidate_list(cx);
            self.retype_sessions(cx);
        }
    }

    /// Settings › Appearance › Text size: store `pref` and set every text
    /// role at it now (task 2.10).
    pub(crate) fn set_text_size_pref(
        &mut self,
        pref: crate::layout::TextSizePref,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        crate::layout::Layout::update(cx, |layout| layout.text_size = pref);
        if cx.aui().text_size != pref.text_size() {
            aui_tokens::AuiTheme::set_text_size(pref.text_size(), window, cx);
            self.invalidate_list(cx);
            self.retype_sessions(cx);
        }
    }

    /// Every live transcript (open, parked, the browser chat) measures its
    /// rows again at the new type: see [`crate::session::SessionView::retype`].
    fn retype_sessions(&mut self, cx: &mut Context<Self>) {
        for view in self.registry.live(self.active.as_ref()) {
            view.update(cx, |view, cx| view.retype(cx));
        }
    }

    /// The Settings → Browser page's Chromium row: leave Settings for
    /// Browse and show `chrome://settings` in a person tab, where
    /// Chromium owns passwords, cookies and site settings. An existing
    /// `chrome://settings` tab is selected instead of opening another.
    pub(crate) fn open_chromium_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(cx);
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.ensure_tabs_loaded(cx);
        let space = self.browser.read(cx).browse_space();
        let existing = self
            .browser
            .read(cx)
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .find(|tab| tab.url == CHROMIUM_SETTINGS_URL)
            .map(|tab| tab.id);
        match existing {
            Some(id) => self.select_browse_sidebar_tab(id, window, cx),
            None => {
                if let Err(reason) = self.open_new_tab(CHROMIUM_SETTINGS_URL, window, cx) {
                    crate::baaz_log!("open_chromium_settings: no tab opened: {reason}");
                }
            }
        }
    }

    #[cfg(feature = "probe")]
    /// `settings-set:<key>=<value>`: the scripted twin of the Settings
    /// rows that are not plain switches. `search-engine=<name>`,
    /// `summary-line=<on|off>`, `theme=<system|light|dark>`,
    /// `density=<comfortable|compact>` and `text-size=<default|large>` land
    /// instantly; an unknown key or a value that does not parse is an
    /// `Err`, never a silent no-op.
    pub(crate) fn step_settings_set(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (key, value) =
            rest.split_once('=').map(|(key, value)| (key.trim(), value.trim())).unwrap_or((rest.trim(), ""));
        match key {
            "search-engine" => match SearchEngine::parse(value) {
                Some(engine) => {
                    set_search_engine(engine, cx);
                    Ok(())
                }
                None => Err(format!("unknown search engine {value:?}")),
            },
            "summary-line" => {
                let on = match value {
                    "on" => true,
                    "off" => false,
                    _ => return Err(format!("summary-line wants on or off, not {value:?}")),
                };
                self.flip_setting("summary_line", on, cx);
                Ok(())
            }
            "theme" => match crate::layout::ThemePref::parse(value) {
                Some(pref) => {
                    self.set_theme_pref(pref, None, cx);
                    Ok(())
                }
                None => Err(format!("theme wants system, light or dark, not {value:?}")),
            },
            "density" => match crate::layout::DensityPref::parse(value) {
                Some(pref) => {
                    self.set_density_pref(pref, None, cx);
                    Ok(())
                }
                None => Err(format!("density wants comfortable or compact, not {value:?}")),
            },
            "text-size" => match crate::layout::TextSizePref::parse(value) {
                Some(pref) => {
                    self.set_text_size_pref(pref, None, cx);
                    Ok(())
                }
                None => Err(format!("text-size wants default or large, not {value:?}")),
            },
            _ => Err(format!(
                "unknown settings key {key:?} (expected search-engine=<name>, summary-line=<on|off>, theme=<system|light|dark>, density=<comfortable|compact> or text-size=<default|large>)"
            )),
        }
    }

    #[cfg(feature = "probe")]
    /// `auto-title`: flip the automatic-naming switch and persist it. The
    /// switch itself lives on the Settings page's General section; this
    /// verb is what captures flip. Off means no title model call ever.
    /// Always flips: `Ok`.
    pub(crate) fn step_auto_title(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let on = !crate::layout::Layout::get(cx).auto_title;
        if !crate::layout::Layout::update(cx, |layout| apply_setting(layout, "auto_title", on)) {
            return Err("no auto_title switch to flip".to_owned());
        }
        self.invalidate_list(cx);
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `auto-summary`: flip the sidebar-summaries switch and persist it.
    /// See [`Self::step_auto_title`]. Off means no rewrite model call ever;
    /// the ladder's preview rung still fills every second line. Always flips: `Ok`.
    pub(crate) fn step_auto_summary(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let on = !crate::layout::Layout::get(cx).auto_summary;
        if !crate::layout::Layout::update(cx, |layout| apply_setting(layout, "auto_summary", on)) {
            return Err("no auto_summary switch to flip".to_owned());
        }
        self.invalidate_list(cx);
        Ok(())
    }

    /// Whether the Settings page stands open (`Route::Settings`).
    pub(crate) fn settings_open(&self, cx: &App) -> bool {
        self.settings_page.read(cx).is_open()
    }

    /// The open page's id (`general`, `providers/codex`), empty while shut.
    /// The probe's crumb verb and the tests read it.
    #[cfg(feature = "probe")]
    pub(crate) fn settings_section(&self, cx: &App) -> String {
        self.settings_page.read(cx).state.section.clone()
    }

    /// What the page needs from the root now (see [`SettingsFacts`]): only
    /// the open section's data is gathered; a shut page gets the defaults.
    pub(crate) fn settings_facts(&self, cx: &App) -> SettingsFacts {
        let page = self.settings_page.read(cx);
        if !page.state.open {
            return SettingsFacts::default();
        }
        let section = page.state.section.as_str();
        let mut facts = SettingsFacts {
            new_provider: self.new_provider_id(cx),
            ..SettingsFacts::default()
        };
        match section {
            "archived" => {
                let mut archived: Vec<&crate::sidebar::SessionEntry> = self.session_list.read(cx).sessions.iter().filter(|e| e.archived).collect();
                archived.sort_by(|a, b| a.updated.cmp(&b.updated).reverse());
                facts.archived = archived
                    .into_iter()
                    .map(|entry| ArchivedRow {
                        id: entry.id.clone(),
                        label: entry.label.clone(),
                        project: entry.project_name.clone().unwrap_or_else(|| "Unfiled".to_owned()),
                        updated: entry.updated,
                    })
                    .collect();
            }
            "browser" => {
                facts.default_browser = self.default_browser_handler(cx).map(str::to_owned);
                facts.default_browser_pending = self.browser.read(cx).default_browser_confirm_pending;
                let data = self.browser.read(cx).data.read(cx);
                facts.history_pages = data.history_len();
                facts.excluded_sites = data.excluded().to_vec();
                facts.sites = data.sites().clone();
            }
            _ if site_subpage_origin(section).is_some() => {
                facts.sites = self.browser.read(cx).data.read(cx).sites().clone();
            }
            "providers" => {
                let empty: &[crate::session_migration::PlannedMove] = &[];
                let plan = self.session_list.read(cx).migration.cache.as_ref().map(|cache| cache.plan_for_render()).unwrap_or(empty);
                if !plan.is_empty() {
                    let (n_claude, n_codex) = crate::session_migration::session_counts(plan);
                    facts.migration = Some(MigrationRow {
                        title: crate::session_migration::prompt_title(n_claude, n_codex),
                        paths: crate::session_migration::row_paths(plan),
                    });
                }
            }
            _ => {
                if let Some(id) = provider_subpage_id(section) {
                    facts.import_line = self.import_card_line(id, cx);
                }
            }
        }
        facts
    }

    /// Run `f` on the page with fresh facts: every root call into the page
    /// that reads them goes through here.
    pub(crate) fn with_settings_page<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut SettingsPage, &mut Context<SettingsPage>) -> R,
    ) -> R {
        let facts = self.settings_facts(cx);
        self.settings_page.update(cx, |page, cx| {
            page.facts = facts;
            f(page, cx)
        })
    }

    /// The page's three slots for this frame, facts pushed first: its
    /// section list for the sidebar column, its surface for the centre
    /// (composed uncached: the facts arrive during this render), and the
    /// bar (the root's sidebar toggle, the page's crumbs and close).
    pub(crate) fn compose_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> (AnyElement, AnyElement, crate::app::BarChrome) {
        let lead = self.render_sidebar_toggle(cx);
        let (nav, title, right) = self.with_settings_page(cx, |page, cx| {
            (page.render_nav(cx), page.render_crumbs(cx), page.actions(window, cx))
        });
        let chrome = crate::app::BarChrome { lead, switch: None, title: Some(title), right };
        (nav, self.settings_page.clone().into_any_element(), chrome)
    }

    /// One event from the page, mapped to the root's own method.
    pub(crate) fn on_settings_event(
        &mut self,
        _page: &Entity<SettingsPage>,
        event: &SettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsEvent::Navigate(target) => self.open_settings_page(target, cx),
            SettingsEvent::Close => self.close_settings(cx),
            SettingsEvent::Flip { id, on } => self.flip_setting(id, *on, cx),
            SettingsEvent::Theme(pref) => self.set_theme_pref(*pref, Some(window), cx),
            SettingsEvent::TextSize(pref) => self.set_text_size_pref(*pref, Some(window), cx),
            SettingsEvent::Density(pref) => self.set_density_pref(*pref, Some(window), cx),
            SettingsEvent::FlipProvider { id, on } => self.flip_provider_enabled(*id, *on, cx),
            SettingsEvent::ProviderIntent(intent) => self.handle_provider_intent(intent.clone(), window, cx),
            SettingsEvent::SetDefaultProvider(id) => {
                let id = *id;
                self.provider_model.update(cx, |model, _| model.choose_new_provider(id));
                cx.notify();
            }
            SettingsEvent::OpenConnect => self.open_connect_screen(cx),
            SettingsEvent::MigrateSessions => self.confirm_session_migration(cx),
            SettingsEvent::OpenImportSheet => self.open_import_sheet(cx),
            SettingsEvent::Unarchive(id) => self.unarchive_session(id.clone(), cx),
            SettingsEvent::ClearHistory => self.ask_clear_history(cx),
            SettingsEvent::ImportChrome => self.open_chrome_import(cx),
            SettingsEvent::IncludeSite(site) => {
                self.browser.read(cx).data.clone().update(cx, |data, cx| data.include(site, cx));
            }
            SettingsEvent::SetSitePermission { origin, key, answer } => {
                let value = answer.map(|answer| answer.as_str());
                // The store's front changes, and the Browse view (whose
                // frame the page's facts come from) redraws with it.
                self.browser.read(cx).data.clone().update(cx, |data, cx| data.set_site(origin, key, value, cx));
            }
            SettingsEvent::ResetSiteZoom(origin) => self.set_site_zoom(origin, 100, cx),
            SettingsEvent::ClearSite(origin) => {
                self.browser.read(cx).data.clone().update(cx, |data, cx| data.clear_site(origin, cx));
                self.apply_site_zoom_to_open_pages(origin, 100, cx);
                if site_subpage_origin(&self.settings_page.read(cx).state.section) == Some(origin.as_str()) {
                    self.open_settings_page("browser", cx);
                }
            }
            SettingsEvent::MakeDefaultBrowser => {
                if !crate::default_browser::make_baaz_default() {
                    crate::baaz_log!("Couldn't change the default browser (macOS refused the request)");
                }
                // macOS confirms asynchronously: wait for the re-read
                // (~1 s) and the next focus-regain instead of showing a
                // stale handler.
                self.browser.update(cx, |browser, cx| browser.await_default_browser(cx));
            }
            SettingsEvent::OpenChromiumSettings => self.open_chromium_settings(window, cx),
        }
    }
}

/// A page's sections, [`SETTINGS_SECTION_GAP`] apart.
fn settings_sections_stack() -> gpui::Div {
    v_flex().w_full().gap(px(SETTINGS_SECTION_GAP))
}

/// One per-site rule or exception under its group's row: the host as the
/// row's title, the rule's word under it, its action at the right.
fn site_row(host: &str, word: Option<&str>, action: impl IntoElement, _cx: &App) -> AnyElement {
    let row = settings_row(host.to_owned()).control(action);
    match word {
        Some(word) => row.description(word.to_owned()),
        None => row,
    }
    .into_any_element()
}

/// The Settings section that edits one site's settings (docs/82 8.6):
/// `browser/site:<origin>`.
pub(crate) fn site_subpage(origin: &str) -> String {
    format!("browser/site:{origin}")
}

/// The origin a site sub-page edits, or `None` for any other section.
pub(crate) fn site_subpage_origin(section: &str) -> Option<&str> {
    section.strip_prefix("browser/site:").filter(|origin| baaz_browser_core::sites::origin_of(origin).as_deref() == Some(*origin))
}

/// The permission kinds a site's page lists, as (stored key, label): every
/// ask the strip can show, in its order.
pub(crate) const SITE_PERMISSION_ROWS: [(&str, &str); 6] = [
    ("camera", "Camera"),
    ("microphone", "Microphone"),
    ("geolocation", "Location"),
    ("notifications", "Notifications"),
    ("clipboard", "Clipboard"),
    ("midi", "MIDI devices"),
];

/// A site's row under Browser › Site permissions: what is kept, in a
/// line (`Camera allowed · Microphone blocked · Zoom 125 %`).
pub(crate) fn site_summary(sites: &baaz_browser_core::sites::SiteSettings, origin: &str) -> String {
    use baaz_browser_core::sites::Answer;
    let mut parts: Vec<String> = sites
        .answers(origin)
        .into_iter()
        .map(|(key, answer)| {
            let label = SITE_PERMISSION_ROWS.iter().find(|(k, _)| *k == key).map(|(_, label)| (*label).to_owned()).unwrap_or_else(|| "Another device".to_owned());
            format!("{label} {}", if answer == Answer::Allow { "allowed" } else { "blocked" })
        })
        .collect();
    if let Some(zoom) = sites.zoom(origin) {
        parts.push(format!("Zoom {zoom}%"));
    }
    parts.join(" · ")
}

/// Whether the live bindings differ from the shipped defaults (a user
/// rebind, add or unbind): what shows Restore defaults. Pure over the
/// rows, so tests drive it without a window.
pub(crate) fn shortcuts_differ_from_defaults(bindings: &[crate::keymap::EffectiveBinding]) -> bool {
    let mut defaults: Vec<(String, String, Option<String>)> = crate::keymap::KEYMAP
        .iter()
        .filter(|row| row.action != "NoAction")
        .map(|row| (row.action.to_string(), row.keystroke.to_string(), row.context.map(str::to_string)))
        .collect();
    let mut live: Vec<(String, String, Option<String>)> = bindings
        .iter()
        .map(|binding| {
            (
                binding.action.clone(),
                binding.keystroke.clone(),
                binding.context.clone(),
            )
        })
        .collect();
    defaults.sort();
    live.sort();
    defaults != live
}

/// The keymap category, relabelled for people.
pub(crate) fn shortcut_group_label(category: &str) -> &str {
    match category {
        "session" => "Session",
        "composer" => "Composer",
        "pane" => "Panes",
        "terminal" => "Terminal",
        "palette" => "Palette",
        "skills" => "Skills",
        "window" => "Window",
        "turn" => "Turn",
        "picker" => "Picker",
        "search" => "Search",
        "sidebar" => "Sidebar",
        "transcript" => "Transcript",
        "settings" => "Settings",
        "project" => "Project",
        other => other,
    }
}

/// Format a captured keystroke the way the keymap stores it.
fn format_keystroke(keystroke: &gpui::Keystroke) -> SharedString {
    let modifiers = &keystroke.modifiers;
    let mut out = String::new();
    if modifiers.function {
        out.push_str("fn-");
    }
    if modifiers.control {
        out.push_str("ctrl-");
    }
    if modifiers.alt {
        out.push_str("alt-");
    }
    if modifiers.platform {
        out.push_str("cmd-");
    }
    if modifiers.shift {
        out.push_str("shift-");
    }
    out.push_str(&keystroke.key.to_lowercase());
    SharedString::from(out)
}

/// Whether a key-down carries no key of its own — a bare modifier press,
/// which stays armed instead of binding.
fn is_bare_modifier(keystroke: &gpui::Keystroke) -> bool {
    if keystroke.key.is_empty() {
        return true;
    }
    let modifiers = &keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform || modifiers.function {
        return false;
    }
    matches!(
        keystroke.key.as_str(),
        "shift" | "control" | "ctrl" | "alt" | "cmd" | "platform" | "super" | "win" | "fn" | "function"
    )
}

/// Which section `id` names in a row-model list: its index, or the first
/// section when `id` is empty or unknown. Test-only, like the row tables.
#[cfg(test)]
pub(crate) fn settings_section_index(sections: &[SettingsSection], id: &str) -> usize {
    if id.is_empty() {
        return 0;
    }
    sections.iter().position(|s| s.id.as_ref() == id).unwrap_or(0)
}

/// A Shortcuts row's stable id: the action and its context, joined on a
/// separator neither may contain, so [`parse_shortcut_row_id`] recovers both
/// for [`SettingsPage::handle_shortcut`].
pub(crate) fn shortcut_row_id(action: &str, context: Option<&str>) -> String {
    format!("{action}\u{1f}{}", context.unwrap_or(""))
}

/// The `(action, context)` a [`shortcut_row_id`] names. `None` is a corrupt
/// id, which the handler ignores rather than writing.
pub(crate) fn parse_shortcut_row_id(id: &str) -> Option<(String, Option<String>)> {
    let (action, context) = id.split_once('\u{1f}')?;
    if action.is_empty() {
        return None;
    }
    Some((
        action.to_string(),
        if context.is_empty() {
            None
        } else {
            Some(context.to_string())
        },
    ))
}

/// Whether the row may arm: it names a live binding that is not reserved.
/// The dialog never reports `Record` for a reserved row; this keeps a
/// scripted one honest too.
fn shortcut_is_editable(id: &str) -> bool {
    parse_shortcut_row_id(id).is_some_and(|(action, context)| {
        crate::keymap::effective_binding(&action, context.as_deref()).is_some_and(|binding| binding.editable)
    })
}

/// The Settings Shortcuts section: one [`SettingsRow::Shortcut`] per live
/// binding, under a caps heading per keymap category, in table order — so a
/// rebound row stays beside its siblings instead of sinking to the end of
/// the effective list. The row label is the binding's label; the detail is
/// the last refusal on that row, else the reserved reason, else the context
/// when off-global. A final Reserved group lists "Terminal takes the key"
/// ONCE (the terminal's keys folded into one row), never once per key.
/// The row model the tests pin; the page draws its own Shortcuts rows
/// ([`SettingsPage::render_content`]).
#[cfg(test)]
pub(crate) fn shortcuts_section(
    bindings: &[crate::keymap::EffectiveBinding],
    recording: Option<&str>,
    errors: &std::collections::HashMap<String, String>,
) -> SettingsSection {
    let mut ordered: Vec<&crate::keymap::EffectiveBinding> = bindings.iter().collect();
    ordered.sort_by_key(|binding| {
        let category_at = crate::keymap::KEYMAP
            .iter()
            .position(|row| row.category == binding.category)
            .unwrap_or(usize::MAX);
        let action_at = crate::keymap::KEYMAP
            .iter()
            .position(|row| row.action == binding.action)
            .unwrap_or(usize::MAX);
        (category_at, action_at)
    });
    let mut rows = Vec::new();
    let mut last_category: Option<&str> = None;
    for binding in ordered {
        if last_category != Some(binding.category.as_str()) {
            rows.push(SettingsRow::Heading {
                text: SharedString::from(binding.category.clone()),
            });
            last_category = Some(binding.category.as_str());
        }
        let id = shortcut_row_id(&binding.action, binding.context.as_deref());
        let detail = if let Some(error) = errors.get(&id) {
            Some(SharedString::from(error.clone()))
        } else if !binding.editable {
            binding.reserved_reason.clone().map(SharedString::from)
        } else {
            binding.context.as_deref().and_then(context_label).map(SharedString::from)
        };
        rows.push(SettingsRow::Shortcut {
            id: SharedString::from(id.clone()),
            label: SharedString::from(binding.label.clone()),
            detail,
            keystroke: Some(SharedString::from(keystroke_glyphs(&binding.keystroke))),
            recording: recording == Some(id.as_str()),
            editable: binding.editable,
        });
    }
    // The terminal's reserved keys, folded into ONE row: every `NoAction`
    // keystroke the keymap holds, as keycaps, in table order.
    let taken = terminal_taken_keys();
    if !taken.is_empty() {
        let shown: Vec<String> = taken.iter().map(|key| keystroke_glyphs(key)).collect();
        rows.push(SettingsRow::Heading {
            text: SharedString::from("Reserved"),
        });
        rows.push(SettingsRow::Note {
            text: SharedString::from(format!("Terminal takes the key — {}", shown.join(", "))),
        });
    }
    SettingsSection {
        id: SharedString::from("shortcuts"),
        label: SharedString::from("Shortcuts"),
        rows,
    }
}

/// Where a binding fires, in words — or nothing for the global context,
/// which is the default and needs no caption. Internal gpui context names
/// ("AuiRoot", "BaazComposer && menu") are never shown to the person.
fn context_label(context: &str) -> Option<&'static str> {
    let context = context.trim();
    match context {
        "AuiRoot" | "" => None,
        c if c.starts_with("BaazComposer") && c.contains("histup") => Some("In the composer, on the first line"),
        c if c.starts_with("BaazComposer") && c.contains("histdown") => Some("In the composer, on the last line"),
        c if c.starts_with("BaazComposer") && c.contains("&& menu") => Some("In the composer, while a menu is open"),
        c if c.starts_with("BaazComposer") => Some("In the composer"),
        c if c.starts_with("BaazTerminal") => Some("In the terminal"),
        c if c.starts_with("BaazRename") => Some("While renaming"),
        c if c.starts_with("AuiMenu") => Some("In the palette"),
        _ => None,
    }
}

/// A keystroke as macOS writes it on a keycap: `cmd-shift-o` → `⌘⇧O`,
/// chords separated by a space. What is stored and matched stays gpui's
/// spelling; this is display only.
fn keystroke_glyphs(keystroke: &str) -> String {
    keystroke
        .split_whitespace()
        .map(|stroke| {
            let mut out = String::new();
            let parts: Vec<&str> = stroke.split('-').collect();
            let (mods, key) = match parts.split_last() {
                // `cmd--` (minus) splits into ["cmd", "", ""]
                Some((last, rest)) if last.is_empty() && !rest.is_empty() => (&rest[..rest.len() - 1], "-"),
                Some((last, rest)) => (rest, *last),
                None => (&parts[..0], ""),
            };
            for m in ["ctrl", "alt", "shift", "cmd"] {
                if mods.contains(&m) {
                    out.push_str(match m {
                        "ctrl" => "\u{2303}",
                        "alt" => "\u{2325}",
                        "shift" => "\u{21e7}",
                        _ => "\u{2318}",
                    });
                }
            }
            let key = match key {
                "enter" => "\u{21a9}".to_owned(),
                "escape" => "esc".to_owned(),
                "backspace" => "\u{232b}".to_owned(),
                "tab" => "\u{21e5}".to_owned(),
                "up" => "\u{2191}".to_owned(),
                "down" => "\u{2193}".to_owned(),
                "left" => "\u{2190}".to_owned(),
                "right" => "\u{2192}".to_owned(),
                "space" => "space".to_owned(),
                k if k.chars().count() == 1 => k.to_uppercase(),
                k => k.to_owned(),
            };
            out.push_str(&key);
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections() -> Vec<SettingsSection> {
        vec![
            SettingsSection {
                id: SharedString::from("sidebar"),
                label: SharedString::from("Sidebar"),
                rows: Vec::new(),
            },
            SettingsSection {
                id: SharedString::from("appearance"),
                label: SharedString::from("Appearance"),
                rows: Vec::new(),
            },
        ]
    }

    /// docs/82 8.6: a site's page is a Browser sub-page, titled and
    /// crumbed by its host, and only a real web origin names one.
    #[test]
    fn site_pages_nest_under_browser() {
        let page = site_subpage("https://cam.example");
        assert_eq!(normalize_settings_target(&page, ""), page, "the target survives");
        assert_eq!(settings_parent(&page), "browser");
        assert_eq!(settings_breadcrumb(&page), ["Settings", "Browser", "cam.example"]);
        assert_eq!(settings_crumb_target(&page, 1), Some("browser"));
        assert_eq!(site_subpage_origin("browser/site:about:blank"), None, "no site, no page");
        assert_eq!(normalize_settings_target("browser/site:nonsense", ""), "general");
        let sites = baaz_browser_core::sites::SiteSettings::from_rows([
            ("https://cam.example".to_owned(), "camera".to_owned(), "allow".to_owned()),
            ("https://cam.example".to_owned(), "microphone".to_owned(), "block".to_owned()),
            ("https://cam.example".to_owned(), "zoom".to_owned(), "125".to_owned()),
        ]);
        assert_eq!(site_summary(&sites, "https://cam.example"), "Camera allowed · Microphone blocked · Zoom 125%");
    }

    #[test]
    fn an_empty_or_unknown_section_id_opens_the_first_section() {
        let sections = sections();
        assert_eq!(settings_section_index(&sections, ""), 0);
        assert_eq!(settings_section_index(&sections, "nope"), 0);
    }

    #[test]
    fn a_section_id_selects_its_section() {
        let sections = sections();
        assert_eq!(settings_section_index(&sections, "sidebar"), 0);
        assert_eq!(settings_section_index(&sections, "appearance"), 1);
    }

    /// Every Sidebar switch flips by row id, including the two auto
    /// switches — and an unknown id is ignored, never a new field by
    /// accident.
    #[test]
    fn every_sidebar_switch_flips_by_id() {
        let mut layout = Layout::default();
        // The retired chevron switch (docs/83 §9, 16 Nov) is an unknown id.
        assert!(!apply_setting(&mut layout, "group_chevron", true));
        assert!(apply_setting(&mut layout, "summary_line", true));
        assert!(layout.summary_line);
        assert!(apply_setting(&mut layout, "summary_line", false));
        assert!(!layout.summary_line);
        assert!(apply_setting(&mut layout, "group_branch", true));
        assert!(layout.group_branch);
        assert!(apply_setting(&mut layout, "auto_title", false));
        assert!(!layout.auto_title);
        assert!(apply_setting(&mut layout, "auto_summary", false));
        assert!(!layout.auto_summary);
        assert!(apply_setting(&mut layout, "handoff_model_summary", false));
        assert!(!layout.handoff_model_summary);
        assert!(!apply_setting(&mut layout, "feedback_mode", true), "the retired feedback switch is no setting");
        assert!(!apply_setting(&mut layout, "nope", true));
    }

    /// B2a-H6: the Browser section is registered — listed after
    /// Sidebar (and Notifications, task 3.5), normalised, crumbed, blurbed,
    /// and home to its rows.
    #[test]
    fn browser_section_is_registered() {
        let ids: Vec<&str> = SETTINGS_SECTIONS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["general", "appearance", "sidebar", "notifications", "browser", "providers", "shortcuts", "archived"]);
        assert_eq!(normalize_settings_target("browser", ""), "browser");
        assert_eq!(normalize_settings_target("BROWSER", ""), "browser");
        assert_eq!(normalize_settings_target("search-engine", ""), "browser");
        assert_eq!(
            settings_breadcrumb("browser"),
            vec!["Settings".to_owned(), "Browser".to_owned()]
        );
        assert_eq!(settings_blurb("browser"), "How Baaz browses: the assistant, tabs, search, downloads.");
        assert_eq!(setting_home("search-engine"), Some("browser"));
        assert_eq!(setting_home("download-dir"), Some("browser"));
    }

    /// Task 3.5: Notifications is a section (after Sidebar), its switches
    /// open it, each changes only its own preference, and an old layout
    /// reads every event on, the badge on, no sound.
    #[test]
    fn notifications_section_and_switches() {
        assert_eq!(normalize_settings_target("notifications", ""), "notifications");
        assert_eq!(normalize_settings_target("notify_sound", ""), "notifications");
        assert!(settings_target_known("notifications"));
        assert_eq!(settings_breadcrumb("notifications"), vec!["Settings".to_owned(), "Notifications".to_owned()]);
        let mut layout = Layout::default();
        assert_eq!(layout.notify_prefs(), crate::attention::NotifyPrefs::default());
        let old: Layout = serde_json::from_str("{}").unwrap();
        assert_eq!(old.notify_prefs(), crate::attention::NotifyPrefs::default());
        assert!(apply_setting(&mut layout, "notify_sound", true));
        assert!(apply_setting(&mut layout, "notify_done", false));
        let prefs = layout.notify_prefs();
        assert!(prefs.sound && !prefs.done && prefs.needs_you && prefs.failed && prefs.badge);
        let text = serde_json::to_string(&layout).unwrap();
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!(back.notify_prefs(), prefs, "the switches persist");
    }

    /// Task 2.9: Appearance is registered after General — normalised,
    /// crumbed, blurbed, and home to the theme and the summary line, which
    /// left Sidebar (one control, one place).
    #[test]
    fn appearance_section_is_registered() {
        assert_eq!(normalize_settings_target("appearance", ""), "appearance");
        assert_eq!(normalize_settings_target("theme", ""), "appearance");
        assert_eq!(normalize_settings_target("summary_line", ""), "appearance");
        assert!(settings_target_known("appearance"));
        assert_eq!(settings_breadcrumb("appearance"), vec!["Settings".to_owned(), "Appearance".to_owned()]);
        assert_eq!(settings_blurb("appearance"), "How Baaz looks. Changes apply live.");
        let layout = Layout::default();
        let sidebar: Vec<String> = sidebar_settings_rows(&layout)
            .iter()
            .filter_map(|row| match row {
                SettingsRow::Switch { id, .. } => Some(id.to_string()),
                _ => None,
            })
            .collect();
        assert!(!sidebar.contains(&"summary_line".to_owned()), "the summary line lives on Appearance only");
    }

    /// A row id round-trips to the action and context the handler writes
    /// with — and a corrupt id parses to nothing, which the handler ignores.
    #[test]
    fn a_shortcut_row_id_round_trips_action_and_context() {
        assert_eq!(
            parse_shortcut_row_id(&shortcut_row_id("NewSession", Some("AuiRoot"))),
            Some(("NewSession".to_string(), Some("AuiRoot".to_string())))
        );
        assert_eq!(
            parse_shortcut_row_id(&shortcut_row_id("OpenSettings", None)),
            Some(("OpenSettings".to_string(), None))
        );
        assert_eq!(parse_shortcut_row_id("no-separator"), None);
        assert_eq!(parse_shortcut_row_id("\u{1f}AuiRoot"), None);
    }

    /// Holds [`crate::store::test_env_lock`] while a test points
    /// `BAAZ_STATE_DIR` at a fresh temp dir, restoring both after: the
    /// variables are process-global, so two such tests at once would read
    /// each other's state.
    struct ShortcutEnv {
        _guard: std::sync::MutexGuard<'static, ()>,
        state_dir: Option<std::ffi::OsString>,
        deterministic: Option<std::ffi::OsString>,
    }

    impl ShortcutEnv {
        fn hold(name: &str) -> (Self, std::path::PathBuf) {
            let env = Self {
                _guard: crate::store::test_env_lock(),
                state_dir: std::env::var_os("BAAZ_STATE_DIR"),
                deterministic: std::env::var_os("BAAZ_DETERMINISTIC"),
            };
            std::env::remove_var("BAAZ_DETERMINISTIC");
            let dir = std::env::temp_dir().join(format!("baaz-shortcuts-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("shortcuts test state dir");
            std::env::set_var("BAAZ_STATE_DIR", &dir);
            (env, dir)
        }
    }

    impl Drop for ShortcutEnv {
        fn drop(&mut self) {
            match &self.state_dir {
                Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
                None => std::env::remove_var("BAAZ_STATE_DIR"),
            }
            match &self.deterministic {
                Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
                None => std::env::remove_var("BAAZ_DETERMINISTIC"),
            }
        }
    }

    /// One [`SettingsRow::Shortcut`] per live binding, carrying its label:
    /// the section lists every default binding, under a heading per
    /// category, plus one final Reserved group folding the terminal's
    /// taken keys into a single note row.
    #[test]
    fn the_shortcuts_section_lists_every_default_binding_with_its_label() {
        let (_env, _dir) = ShortcutEnv::hold("section");
        let bindings = crate::keymap::effective_bindings();
        assert!(!bindings.is_empty(), "the defaults list something");
        let section = shortcuts_section(&bindings, None, &std::collections::HashMap::new());
        assert_eq!(section.id.as_ref(), "shortcuts");
        let mut shortcuts = Vec::new();
        let mut headings = Vec::new();
        let mut notes = Vec::new();
        for row in &section.rows {
            match row {
                SettingsRow::Shortcut { id, label, keystroke, .. } => {
                    shortcuts.push((id.to_string(), label.to_string(), keystroke.clone()));
                }
                SettingsRow::Heading { text } => headings.push(text.to_string()),
                SettingsRow::Note { text } => notes.push(text.to_string()),
                SettingsRow::Switch { .. } => {
                    panic!("the Shortcuts section holds no switches");
                }
            }
        }
        assert_eq!(
            shortcuts.len(),
            bindings.len(),
            "one row per live binding, no more and no fewer"
        );
        for binding in &bindings {
            let id = shortcut_row_id(&binding.action, binding.context.as_deref());
            let (_, label, keystroke) = shortcuts
                .iter()
                .find(|(row_id, _, _)| row_id == &id)
                .unwrap_or_else(|| panic!("no row for {} in {:?}", binding.action, shortcuts));
            assert_eq!(label, &binding.label, "the row carries the binding's label");
            assert_eq!(
                keystroke.as_deref(),
                Some(keystroke_glyphs(&binding.keystroke).as_str()),
                "the row shows the live keystroke as keycaps"
            );
        }
        let mut categories: Vec<&str> = Vec::new();
        for binding in &bindings {
            if !categories.contains(&binding.category.as_str()) {
                categories.push(binding.category.as_str());
            }
        }
        let mut expected: Vec<String> = categories.iter().map(|item| item.to_string()).collect();
        expected.push("Reserved".to_owned());
        assert_eq!(headings, expected, "one heading per category, then the Reserved group, last");
        // The Reserved group is exactly one note row naming every taken key.
        assert_eq!(notes.len(), 1, "the terminal's keys fold into a single row");
        let taken = terminal_taken_keys();
        assert!(!taken.is_empty(), "the keymap holds terminal-taken keys");
        for key in &taken {
            assert!(
                notes[0].contains(&keystroke_glyphs(key)),
                "the Reserved row names {key}"
            );
        }
        assert_eq!(
            notes[0].match_indices("Terminal takes the key").count(),
            1,
            "listed ONCE, never once per key"
        );
    }

    /// Reserved rows are read-only with their reason as detail; an
    /// off-global row names its context; a global row has no detail; exactly
    /// one recording row arms.
    #[test]
    fn shortcut_rows_mark_recording_reserved_and_context() {
        let (_env, _dir) = ShortcutEnv::hold("rows");
        let bindings = crate::keymap::effective_bindings();
        let recording = shortcut_row_id("NewSession", Some("AuiRoot"));
        let section = shortcuts_section(&bindings, Some(&recording), &std::collections::HashMap::new());
        let mut saw_recording = 0;
        for row in &section.rows {
            if let SettingsRow::Shortcut { id, detail, recording: armed, editable, .. } = row {
                if *armed {
                    saw_recording += 1;
                    assert_eq!(id.as_ref(), recording, "only the named row arms");
                }
                let parsed = parse_shortcut_row_id(id).expect("every row id parses");
                let binding = bindings
                    .iter()
                    .find(|binding| {
                        binding.action == parsed.0 && binding.context == parsed.1
                    })
                    .expect("every row names a live binding");
                assert_eq!(*editable, binding.editable);
                if !binding.editable {
                    let reason = binding.reserved_reason.clone().expect("a reserved row says why");
                    assert_eq!(detail.as_deref(), Some(reason.as_str()), "reserved detail is the reason");
                } else if binding.action == "OpenSettings" {
                    assert_eq!(detail, &None, "a global row has no detail");
                } else if binding.action == "NewSession" {
                    assert_eq!(detail, &None, "the root context is global and says nothing");
                } else if let Some(context) = binding.context.as_deref() {
                    assert_eq!(detail.as_deref(), context_label(context), "contexts read as words");
                }
            }
        }
        assert_eq!(saw_recording, 1, "exactly one row arms at a time");
        let reserved = bindings.iter().find(|binding| !binding.editable).expect("a default is reserved");
        assert!(!reserved.reserved_reason.clone().unwrap_or_default().is_empty());
    }

    /// A refusal hangs on the row's detail, over whatever it showed before.
    #[test]
    fn a_shortcut_error_becomes_the_row_detail() {
        let (_env, _dir) = ShortcutEnv::hold("error");
        let bindings = crate::keymap::effective_bindings();
        let id = shortcut_row_id("NewSession", Some("AuiRoot"));
        let mut errors = std::collections::HashMap::new();
        errors.insert(id.clone(), "cmd-q is reserved".to_string());
        let section = shortcuts_section(&bindings, None, &errors);
        let detail = section.rows.iter().find_map(|row| match row {
            SettingsRow::Shortcut { id: row_id, detail, .. } if row_id.as_ref() == id => detail.clone(),
            _ => None,
        });
        assert_eq!(detail.as_deref(), Some("cmd-q is reserved"));
    }

    /// Off means no model call ever for that feature: the flipped layout
    /// straight into both pure decisions.
    #[test]
    fn a_switch_off_blocks_its_model_call() {
        let mut layout = Layout::default();
        apply_setting(&mut layout, "auto_title", false);
        assert!(!crate::titles::should_title(layout.auto_title, true, None, 0, false));
        apply_setting(&mut layout, "auto_summary", false);
        assert!(!crate::byline::should_rewrite(
            layout.auto_summary,
            false,
            true,
            (None, None),
            (Some("fix it"), None),
            None,
            std::time::Instant::now(),
        ));
    }

    #[test]
    fn keystrokes_read_as_keycaps() {
        // macOS order: control, option, shift, command.
        assert_eq!(keystroke_glyphs("cmd-shift-o"), "\u{21e7}\u{2318}O");
        assert_eq!(keystroke_glyphs("ctrl-c"), "\u{2303}C");
        assert_eq!(keystroke_glyphs("cmd-,"), "\u{2318},");
        assert_eq!(keystroke_glyphs("cmd--"), "\u{2318}-");
        assert_eq!(keystroke_glyphs("enter"), "\u{21a9}");
        assert_eq!(keystroke_glyphs("cmd-k cmd-s"), "\u{2318}K \u{2318}S");
    }

    #[test]
    fn contexts_read_as_words_and_the_global_one_is_silent() {
        assert_eq!(context_label("AuiRoot"), None);
        assert_eq!(context_label("AuiMenu > Input"), Some("In the palette"));
        assert_eq!(context_label("BaazComposer && menu"), Some("In the composer, while a menu is open"));
        assert_eq!(context_label("BaazRename"), Some("While renaming"));
        assert_eq!(context_label("SomethingNew"), None);
    }

    /// Deep links land on the right page: bare sections, `providers/<wire>`
    /// sub-pages, empty (last-visited, else General) and unknown (General).
    /// Step verbs stay idempotent — open, never toggle — so parsing twice
    /// agrees with parsing once.
    #[test]
    fn settings_targets_normalize_to_pages() {
        assert_eq!(normalize_settings_target("general", ""), "general");
        assert_eq!(normalize_settings_target("sidebar", ""), "sidebar");
        assert_eq!(normalize_settings_target("providers", ""), "providers");
        assert_eq!(normalize_settings_target("shortcuts", ""), "shortcuts");
        assert_eq!(normalize_settings_target("archived", ""), "archived");
        assert_eq!(normalize_settings_target("providers/codex", ""), "providers/codex");
        assert_eq!(normalize_settings_target("providers/claude-code", ""), "providers/claude-code");
        assert_eq!(normalize_settings_target("providers/muse", ""), "providers/muse");
        assert_eq!(normalize_settings_target("", ""), "general");
        assert_eq!(normalize_settings_target("", "shortcuts"), "shortcuts");
        assert_eq!(normalize_settings_target("nope", ""), "general");
        assert_eq!(normalize_settings_target("providers/nope", ""), "general");
        // A row id opens its home page.
        assert_eq!(normalize_settings_target("auto_title", ""), "general");
        assert_eq!(normalize_settings_target("summary_line", ""), "appearance");
        assert_eq!(normalize_settings_target("fold_finished_turns", ""), "general");
        assert_eq!(normalize_settings_target("provider-enabled:codex", ""), "providers");
        assert_eq!(normalize_settings_target("use-own-mcp:codex", ""), "providers");
        assert_eq!(normalize_settings_target("shortcut:NewSession", ""), "shortcuts");
        assert_eq!(normalize_settings_target("archived:s-1", ""), "archived");
        // Idempotent: normalizing the normalized target is a fixed point.
        for target in ["general", "providers/codex", "archived"] {
            assert_eq!(normalize_settings_target(target, ""), target);
        }
    }

    /// Sub-pages select Providers in the section list and extend the
    /// breadcrumb; top-level pages crumb themselves.
    #[test]
    fn sub_pages_nest_under_providers() {
        assert_eq!(settings_parent("providers/codex"), "providers");
        assert_eq!(settings_parent("providers"), "providers");
        assert_eq!(settings_parent("general"), "general");
        assert_eq!(
            settings_breadcrumb("providers/codex"),
            vec!["Settings".to_owned(), "Providers".to_owned(), "Codex".to_owned()]
        );
        assert_eq!(
            settings_breadcrumb("general"),
            vec!["Settings".to_owned(), "General".to_owned()]
        );
        assert!(is_provider_subpage("providers/muse"));
        assert!(!is_provider_subpage("providers"));
    }
    /// Every crumb but the page's own opens its page (owner, 2026-10-29).
    #[test]
    fn breadcrumbs_link_to_their_pages() {
        assert_eq!(settings_crumb_target("providers/codex", 0), Some("general"));
        assert_eq!(settings_crumb_target("providers/codex", 1), Some("providers"));
        assert_eq!(settings_crumb_target("providers/codex", 2), None, "the page itself is no link");
        assert_eq!(settings_crumb_target("browser", 0), Some("general"));
        assert_eq!(settings_crumb_target("browser", 1), None);
    }


    /// Every setting id the pages render lives on exactly one page: the
    /// row models themselves are enumerated — General and Sidebar from
    /// the switch builders the sections render, Providers from the
    /// section's own rows, each provider sub-page from its card plus its
    /// opt-in switch, Shortcuts from the live keymap rows, Archived from
    /// its link spelling — and each id must both sit on exactly one page
    /// and agree with [`setting_home`]. Unknown ids name no page.
    #[test]
    fn every_rendered_setting_id_has_exactly_one_page() {
        use crate::providers::ProviderId;
        let (_env, _dir) = ShortcutEnv::hold("coverage");
        let layout = Layout::default();
        let switch_ids = |rows: &[SettingsRow]| {
            rows.iter()
                .filter_map(|row| match row {
                    SettingsRow::Switch { id, .. } => Some(id.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // Page id → the setting ids that page renders.
        let mut pages: Vec<(String, Vec<String>)> = vec![
            ("general".to_owned(), switch_ids(&general_settings_rows(&layout))),
            ("appearance".to_owned(), switch_ids(&appearance_settings_rows(&layout))),
            ("sidebar".to_owned(), switch_ids(&sidebar_settings_rows(&layout))),
            ("providers".to_owned(), switch_ids(&crate::settings_providers::providers_section().rows)),
            ("archived".to_owned(), vec!["archived:s-1".to_owned()]),
        ];
        for provider in ProviderId::all() {
            let wire = provider.as_str();
            let mut rows = vec![format!("provider-card:{wire}")];
            if crate::settings_providers::own_mcp_providers().contains(&provider) {
                rows.push(format!("use-own-mcp:{wire}"));
            }
            pages.push((format!("providers/{wire}"), rows));
        }
        let bindings = crate::keymap::effective_bindings();
        assert!(!bindings.is_empty(), "the defaults list something");
        let shortcuts = shortcuts_section(&bindings, None, &std::collections::HashMap::new());
        let mut shortcut_ids = Vec::new();
        for row in &shortcuts.rows {
            if let SettingsRow::Shortcut { id, .. } = row {
                shortcut_ids.push(id.to_string());
            }
        }
        assert_eq!(
            shortcut_ids.len(),
            bindings.len(),
            "one rendered row per live binding"
        );
        pages.push(("shortcuts".to_owned(), shortcut_ids));
        // The deep-link spellings open their home page too.
        let links = [
            ("shortcut:NewSession", "shortcuts"),
            ("provider-enabled:codex", "providers"),
            ("use-own-mcp:codex", "providers/*"),
            ("auto_title", "general"),
            ("theme", "appearance"),
        ];
        for (id, home) in links {
            assert_eq!(setting_home(id), Some(home), "the link home of {id}");
        }
        // Every rendered id sits on exactly one page and agrees with
        // `setting_home` (sub-pages report the `providers/*` family).
        let mut seen: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for (page, ids) in &pages {
            assert!(!ids.is_empty(), "{page} renders no setting ids");
            for id in ids {
                seen.entry(id.clone()).or_default().push(page.clone());
            }
        }
        for (id, homes) in &seen {
            assert_eq!(homes.len(), 1, "{id} renders on {homes:?}, want exactly one page");
            let page = &homes[0];
            let expected = if page.starts_with("providers/") { "providers/*" } else { page.as_str() };
            assert_eq!(setting_home(id), Some(expected), "the home of {id}");
        }
        // The layout switches are all covered, and unknown ids name no
        // page.
        for id in [
            "auto_title",
            "auto_summary",
            "handoff_model_summary",
            "fold_finished_turns",
            "archive_on_merge",
            "summary_line",
            "group_branch",
        ] {
            assert!(seen.contains_key(id), "{id} renders somewhere");
        }
        assert_eq!(setting_home("nope"), None);
    }

    /// Escape order: recording → search → sub-page up → close+restore.
    #[test]
    fn settings_escape_cancels_search_and_climbs_before_closing() {
        assert_eq!(settings_escape(true, true, "providers/codex"), SettingsEscape::CancelRecording);
        assert_eq!(settings_escape(false, true, "providers/codex"), SettingsEscape::ClearSearch);
        assert_eq!(settings_escape(false, true, "shortcuts"), SettingsEscape::ClearSearch);
        assert_eq!(
            settings_escape(false, false, "providers/codex"),
            SettingsEscape::UpToProviders
        );
        assert_eq!(settings_escape(false, false, "providers"), SettingsEscape::Close);
        assert_eq!(settings_escape(false, false, "general"), SettingsEscape::Close);
    }

    /// Opening saves the session and closing restores it; reopening an
    /// open page never toggles it shut; ⌘, reopens the last-visited
    /// section. Navigation exits own nothing to restore.
    #[test]
    fn opening_settings_then_closing_restores_the_session() {
        let mut page = SettingsPageState::default();
        page.open("", Some("s-1".to_owned()));
        assert!(page.open);
        assert_eq!(page.section, "general");
        assert_eq!(page.saved_active.as_deref(), Some("s-1"));
        // Idempotent: a second open retargets without toggling or
        // dropping the saved session.
        page.open("providers/codex", Some("s-2".to_owned()));
        assert!(page.open);
        assert_eq!(page.section, "providers/codex");
        assert_eq!(page.saved_active.as_deref(), Some("s-1"));
        // Sub-page up, then close restores.
        page.section = "providers".to_owned();
        assert_eq!(page.close().as_deref(), Some("s-1"));
        assert!(!page.open);
        // ⌘, reopens the last-visited section.
        page.open("", None);
        assert_eq!(page.section, "providers");
        // Navigation exits without a restore.
        page.open("general", Some("s-9".to_owned()));
        page.exit_for_navigation();
        assert!(!page.open);
        assert_eq!(page.saved_active, None);
    }

    /// Restore defaults shows only when the live rows differ from the
    /// shipped table.
    #[test]
    fn restore_defaults_shows_only_when_something_differs() {
        let (_env, _dir) = ShortcutEnv::hold("differ");
        let bindings = crate::keymap::effective_bindings();
        assert!(!shortcuts_differ_from_defaults(&bindings));
        let mut changed = bindings.clone();
        changed.pop();
        assert!(shortcuts_differ_from_defaults(&changed));
    }

}
