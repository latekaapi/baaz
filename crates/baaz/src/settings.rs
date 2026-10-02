//! The Settings page (⌘,, File → Settings…, the account menu's
//! "Settings…" / "Providers…" rows, `--steps settings[:<section>]`,
//! palette, `settings:<section>` deep links).
//!
//! B11: Settings is a full page (`Route::Settings`), not a modal. While it
//! is open the left column shows the section list (with a "← Back" row at
//! its top) instead of the session sidebar; the centre+right columns merge
//! into one settings surface (max 680 px, centred, own scroll, 32 px top
//! padding). The right pane and terminal dock are hidden, not closed —
//! their state is untouched and restored on exit.
//!
//! The six sidebar flags stay in `layout.json` — there is no second store.
//! [`crate::app::Harness::settings_sections`] builds the sections' rows
//! from that state every frame, and a switch intent flips the matching
//! layout field back through [`crate::layout::write`].
//!
//! Extensibility: a later section is one more id in
//! [`normalize_settings_target`] + [`SETTINGS_SECTIONS`], one more id in
//! [`apply_setting`]/[`setting_home`], and one more page arm in
//! [`Harness::render_settings_content`].
//!
//! The Shortcuts section is the exception to that shape: its rows come from
//! [`crate::keymap::effective_bindings`], cached on [`Harness`](crate::app::Harness)
//! (`shortcuts_cache`) and rebuilt per frame, and its edits go through
//! [`Harness::handle_shortcut`](crate::app::Harness::handle_shortcut).

use aui::data::{button, icon_button, ButtonSize};
use aui::overlay::{SettingsRow, SettingsSection, ShortcutEdit};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled};
use gpui::{div, prelude::*, px, AnyElement, Context, Focusable, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::switch::Switch;

use crate::app::Harness;
use crate::layout::Layout;
use crate::providers::ProviderId;

/// Flip one Sidebar switch in `layout` by row id. `false` is an unknown id,
/// ignored by every caller. Pure, so tests drive it without a window;
/// [`Harness::flip_setting`] and the `auto-*` step verbs persist the layout
/// and redraw around it.
pub(crate) fn apply_setting(layout: &mut Layout, id: &str, on: bool) -> bool {
    match id {
        "group_chevron" => layout.group_chevron = on,
        "group_bar" => layout.group_bar = on,
        "group_branch" => layout.group_branch = on,
        "auto_title" => layout.auto_title = on,
        "auto_summary" => layout.auto_summary = on,
        "handoff_model_summary" => layout.handoff_model_summary = on,
        "fold_finished_turns" => layout.fold_finished_turns = on,
        _ => return false,
    }
    true
}

/// The page's sections, in list order (a muted divider before Archived).
pub(crate) const SETTINGS_SECTIONS: &[(&str, &str)] = &[
    ("general", "General"),
    ("sidebar", "Sidebar"),
    ("providers", "Providers"),
    ("shortcuts", "Shortcuts"),
    ("archived", "Archived"),
];

/// One-line descriptions per page: the H1's muted line.
pub(crate) fn settings_blurb(section: &str) -> &'static str {
    match section {
        "general" => "Model housekeeping and app defaults. Changes apply live.",
        "sidebar" => "What the session list shows. Changes apply live.",
        "providers" => "Signing a CLI out signs it out on this Mac.",
        "shortcuts" => "Click a keycap to rebind it. Changes apply live.",
        "archived" => "Sessions you archived, grouped by project.",
        _ if is_provider_subpage(section) => "Account, tools and defaults for this provider.",
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
    let parsed = ProviderId::parse(wire);
    (parsed.as_str() == wire).then_some(parsed)
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
    if provider_subpage_id(&lower).is_some() {
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

/// The section-list selection for a page id: a provider sub-page selects
/// Providers (its sub-pages nest under Providers, indented, only while
/// Providers is selected).
pub(crate) fn settings_parent(section: &str) -> &str {
    if is_provider_subpage(section) {
        "providers"
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
    match section {
        "general" => crumbs.push("General".to_owned()),
        "sidebar" => crumbs.push("Sidebar".to_owned()),
        "providers" => crumbs.push("Providers".to_owned()),
        "shortcuts" => crumbs.push("Shortcuts".to_owned()),
        "archived" => crumbs.push("Archived".to_owned()),
        _ => crumbs.push("General".to_owned()),
    }
    crumbs
}

/// Every former setting's exactly-one home page: the row id → page id.
/// Table-driven so the coverage test enumerates it.
pub(crate) fn setting_home(id: &str) -> Option<&'static str> {
    match id {
        "auto_title" | "auto_summary" | "handoff_model_summary" | "fold_finished_turns" => {
            Some("general")
        }
        "group_chevron" | "group_bar" | "group_branch" => Some("sidebar"),
        _ if crate::settings_providers::parse_enabled_row(id).is_some() => Some("providers"),
        _ if id.starts_with("use-own-mcp:") || id.starts_with("provider-card:") => Some("providers/*"),
        _ if id.starts_with("shortcut:") => Some("shortcuts"),
        _ if id.starts_with("archived:") => Some("archived"),
        _ => None,
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
    /// The page id: `general` | `sidebar` | `providers` |
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

impl Harness {
    /// Every section's rows, built from state each frame: General (the
    /// three model-spending switches plus the fold switch), Sidebar
    /// (chevron/bar/branch), Providers (enable switches), Shortcuts (live
    /// keymap). Archived has no switches — its page lists sessions.
    ///
    /// A later section is one more arm here (and one more `on_switch` id in
    /// [`Self::flip_setting`]).
    pub(crate) fn settings_sections(&self) -> Vec<SettingsSection> {
        vec![
            SettingsSection {
                id: SharedString::from("general"),
                label: SharedString::from("General"),
                rows: vec![
                    SettingsRow::Switch {
                        id: SharedString::from("auto_title"),
                        label: SharedString::from("Name sessions automatically"),
                        detail: Some(SharedString::from(
                            "Spend one cheap call naming a new session after its first message",
                        )),
                        on: self.layout.auto_title,
                    },
                    SettingsRow::Switch {
                        id: SharedString::from("auto_summary"),
                        label: SharedString::from("Summarise sessions in the sidebar"),
                        detail: Some(SharedString::from(
                            "Show the last request beside the last reply; rewrite poor ones",
                        )),
                        on: self.layout.auto_summary,
                    },
                    SettingsRow::Switch {
                        id: SharedString::from("handoff_model_summary"),
                        label: SharedString::from("Summarise handoffs with a model"),
                        detail: Some(SharedString::from(
                            "A cheap model writes the summary the next provider reads (one short turn). Off: the first lines of the earliest replies.",
                        )),
                        on: self.layout.handoff_model_summary,
                    },
                    SettingsRow::Switch {
                        id: SharedString::from("fold_finished_turns"),
                        label: SharedString::from("Fold finished turns"),
                        detail: Some(SharedString::from(
                            "Settled turns fold their work behind one row; the final answer stays out. Off: every run stays expanded.",
                        )),
                        on: self.layout.fold_finished_turns,
                    },
                ],
            },
            SettingsSection {
                id: SharedString::from("sidebar"),
                label: SharedString::from("Sidebar"),
                rows: vec![
                    SettingsRow::Switch {
                        id: SharedString::from("group_chevron"),
                        label: SharedString::from("Collapse chevron"),
                        detail: Some(SharedString::from("Show a chevron on project rows to fold them")),
                        on: self.layout.group_chevron,
                    },
                    SettingsRow::Switch {
                        id: SharedString::from("group_bar"),
                        label: SharedString::from("Current-project bar"),
                        detail: Some(SharedString::from("Mark the open session's project with an accent bar")),
                        on: self.layout.group_bar,
                    },
                    SettingsRow::Switch {
                        id: SharedString::from("group_branch"),
                        label: SharedString::from("Branch name"),
                        detail: Some(SharedString::from("Show each project's git branch on its row")),
                        on: self.layout.group_branch,
                    },
                ],
            },
            crate::settings_providers::providers_section(),
            shortcuts_section(
                &self.shortcuts_cache,
                self.recording_shortcut.as_deref(),
                &self.shortcut_errors,
            ),
        ]
    }

    /// The active session id for the page's restore record, if any.
    fn settings_active_id(&self, cx: &mut Context<Self>) -> Option<String> {
        self.active.as_ref().map(|view| view.read(cx).session_id.clone())
    }

    /// Open the Settings page on the Providers section: where the account
    /// menu's "Providers…" row lands. Idempotent (open, never toggle).
    pub(crate) fn open_providers(&mut self, cx: &mut Context<Self>) {
        self.open_settings_page("providers", cx);
    }

    /// Open the Settings page on legacy dialog index `section` (menu rows,
    /// ⌘,): 0 General, 1 Sidebar, 2 Providers, 3 Shortcuts, 4 Archived.
    /// Idempotent (open, never toggle). Menus, palette and modal go first.
    pub(crate) fn open_settings(&mut self, section: usize, cx: &mut Context<Self>) {
        let target = SETTINGS_SECTIONS.get(section).map(|(id, _)| *id).unwrap_or("general");
        self.open_settings_page(target, cx);
    }

    /// Open the Settings page at `target` (`settings:<target>` spelling).
    /// Saves the active session for close-restore; hides (never writes)
    /// right-pane and terminal state. Opening closes the Skills page, so
    /// only one full-page route stands open.
    pub(crate) fn open_settings_page(&mut self, target: &str, cx: &mut Context<Self>) {
        self.refresh_shortcuts();
        self.recording_shortcut = None;
        let active = self.settings_active_id(cx);
        self.settings_page.open(target, active);
        if self.skills.open {
            self.skills.open = false;
        }
        self.overlays.update(cx, |overlays, _| {
            overlays.menu = None;
            overlays.palette = None;
            overlays.dialog = None;
        });
        // Settings → Providers recomputes the B4M plan off the UI thread:
        // the row renders the cache (never the walker) and follows when
        // the recompute lands.
        if target.split('/').next() == Some(crate::settings_providers::PROVIDERS_SECTION_ID) {
            self.refresh_migration_cache(cx);
        }
        cx.notify();
    }

    /// Re-read the Shortcuts cache, so the section shows the file as it is
    /// now — after a write, or after a hand edit while the page stood open.
    fn refresh_shortcuts(&mut self) {
        self.shortcuts_cache = crate::keymap::effective_bindings();
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
                    self.recording_shortcut = Some(id.to_string());
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
                self.recording_shortcut = None;
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
                self.recording_shortcut = None;
                if accepted {
                    self.refresh_shortcuts();
                    crate::app::bind_keys(cx);
                }
            }
            ShortcutEdit::Cancel => {
                if self.recording_shortcut.as_deref() == Some(id.as_ref()) {
                    self.recording_shortcut = None;
                }
            }
        }
        cx.notify();
    }

    /// Close the Settings page, restoring the saved session. Scroll,
    /// draft, focus, right pane and terminal come back exactly: the page
    /// never wrote their state, it only hid them.
    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_page.close();
        self.recording_shortcut = None;
        cx.notify();
    }

    /// One Escape press on the page (§1.3 order): cancel a recording, clear
    /// the Shortcuts search, go up from a provider sub-page, else close and
    /// restore. Returns `true` when the page stood open.
    pub(crate) fn settings_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.settings_page.open {
            return false;
        }
        match settings_escape(
            self.recording_shortcut.is_some(),
            !self.settings_page.shortcut_search.trim().is_empty(),
            &self.settings_page.section.clone(),
        ) {
            SettingsEscape::CancelRecording => {
                self.recording_shortcut = None;
            }
            SettingsEscape::ClearSearch => {
                self.settings_page.shortcut_search.clear();
            }
            SettingsEscape::UpToProviders => {
                self.settings_page.section = "providers".to_owned();
                self.settings_page.last_visited = "providers".to_owned();
            }
            SettingsEscape::Close => {
                self.close_settings(cx);
                return true;
            }
        }
        cx.notify();
        true
    }

    /// Leave the page for navigation (⌘N, palette jump, session click):
    /// the destination owns the next route, so nothing is restored.
    pub(crate) fn exit_settings_for_navigation(&mut self, cx: &mut Context<Self>) {
        if self.settings_page.open {
            self.settings_page.exit_for_navigation();
            self.recording_shortcut = None;
            cx.notify();
        }
    }

    /// `settings[:<section>]`: open the page, optionally at that section
    /// (`settings:sidebar`, `settings:providers/codex`). Empty reopens the
    /// last-visited section (General on first open); unknown opens General.
    /// Idempotent: open, never toggle.
    pub(crate) fn step_settings(&mut self, rest: &str, cx: &mut Context<Self>) {
        self.open_settings_page(rest.trim(), cx);
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
        if !apply_setting(&mut self.layout, id, on) {
            return;
        }
        crate::layout::write(&self.layout);
        // B12fix: the fold switch applies live to every open view, not
        // just the next frame's settings rows — parked views fold too.
        if id == "fold_finished_turns" {
            self.apply_fold_to_views(on, cx);
        }
        self.invalidate_list();
        cx.notify();
    }

    /// `auto-title`: flip the automatic-naming switch and persist it. The
    /// switch itself lives on the Settings page's General section; this
    /// verb is what captures flip. Off means no title model call ever.
    pub(crate) fn step_auto_title(&mut self, cx: &mut Context<Self>) {
        let on = !self.layout.auto_title;
        apply_setting(&mut self.layout, "auto_title", on);
        crate::layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// `auto-summary`: flip the sidebar-summaries switch and persist it.
    /// See [`Self::step_auto_title`]. Off means no rewrite model call ever;
    /// the ladder's preview rung still fills every second line.
    pub(crate) fn step_auto_summary(&mut self, cx: &mut Context<Self>) {
        let on = !self.layout.auto_summary;
        apply_setting(&mut self.layout, "auto_summary", on);
        crate::layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// The old modal slot: the page renders inline now (left column +
    /// centre surface), so this is always `None`. Kept so the window
    /// root's overlay slot keeps compiling while callers move over.
    pub(crate) fn render_settings(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        None
    }

    /// Whether the Settings page stands open (`Route::Settings`).
    pub(crate) fn settings_open(&self) -> bool {
        self.settings_page.open
    }

    /// The left column while the page is open: a "← Back" row at the top,
    /// then the section list (same width as the sidebar cell that hosts
    /// it). Provider sub-pages nest under Providers, indented, only while
    /// Providers is selected. Every row carries a role and a name.
    pub(crate) fn render_settings_nav(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let selected = settings_parent(&self.settings_page.section);
        let back = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_settings(cx));
        let mut list = v_flex()
            .id("settings-nav")
            .size_full()
            .bg(p.surface_1)
            .child(
                h_flex()
                    .id("settings-back")
                    .flex_none()
                    .items_center()
                    .gap(px(scale::SP_2))
                    .px(px(scale::SP_4))
                    .py(px(scale::SP_3))
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label("Back to sessions")
                    .on_click(back)
                    .child(div().text_color(p.ink_3).ui(scale::FS_13).child("← Back")),
            )
            .child(div().h(px(1.0)).w_full().flex_none().bg(p.line));
        let mut section_list = v_flex()
            .id("settings-sections")
            .w_full()
            .role(gpui::Role::TabList)
            .aria_label("Settings sections");
        for (id, label) in SETTINGS_SECTIONS {
            let target = id.to_string();
            let open = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.open_settings_page(&target, cx);
            });
            let active = selected == *id;
            let mut row = h_flex()
                .id(SharedString::from(format!("settings-nav-{id}")))
                .w_full()
                .items_center()
                .px(px(scale::SP_4))
                .py(px(scale::SP_2))
                .cursor_pointer()
                .role(gpui::Role::Tab)
                .aria_label(format!("Settings section, {label}"))
                .on_click(open)
                .child(
                    div()
                        .flex_1()
                        .ui(scale::FS_13)
                        .text_color(if active { p.ink } else { p.ink_3 })
                        .child(label.to_string()),
                );
            if active {
                row = row.bg(p.surface_2);
            }
            section_list = section_list.child(row);
            // Provider sub-pages nest under Providers, indented, only
            // while Providers is selected.
            if *id == "providers" && selected == "providers" {
                for provider in ProviderId::all() {
                    let sub = format!("providers/{}", provider.as_str());
                    let open_sub = cx.listener({
                        let sub = sub.clone();
                        move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            this.open_settings_page(&sub, cx);
                        }
                    });
                    let sub_active = self.settings_page.section == sub;
                    let mut sub_row = h_flex()
                        .id(SharedString::from(format!("settings-nav-{}", provider.as_str())))
                        .w_full()
                        .items_center()
                        .pl(px(scale::SP_4 + 16.0))
                        .pr(px(scale::SP_4))
                        .py(px(scale::SP_2))
                        .cursor_pointer()
                        .role(gpui::Role::Tab)
                        .aria_label(format!("Settings section, {} provider", provider.label()))
                        .on_click(open_sub)
                        .child(
                            div()
                                .flex_1()
                                .ui(scale::FS_13)
                                .text_color(if sub_active { p.ink } else { p.ink_3 })
                                .child(provider.label().to_string()),
                        );
                    if sub_active {
                        sub_row = sub_row.bg(p.surface_2);
                    }
                    section_list = section_list.child(sub_row);
                }
            }
            // A muted divider before Archived.
            if *id == "shortcuts" {
                section_list = section_list.child(
                    div().h(px(1.0)).w_full().flex_none().my(px(scale::SP_2)).bg(p.line),
                );
            }
        }
        list = list.child(section_list);
        list.into_any_element()
    }

    /// The centre header cell while the page is open: the breadcrumb
    /// (`Settings / Providers / Codex`) as a navigation landmark, a
    /// spacer, and one ghost close button labelled "Close settings".
    pub(crate) fn render_settings_centre_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let crumbs = settings_breadcrumb(&self.settings_page.section);
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_settings(cx));
        let mut trail = h_flex().id("settings-breadcrumb").flex_none().items_center().gap(px(scale::SP_2)).role(gpui::Role::Navigation).aria_label(format!(
            "Breadcrumb, {}",
            crumbs.join(" / ")
        ));
        for (index, crumb) in crumbs.iter().enumerate() {
            if index > 0 {
                trail = trail.child(div().flex_none().ui(scale::FS_13).text_color(p.ink_4).child("/"));
            }
            let last = index + 1 == crumbs.len();
            trail = trail.child(
                div()
                    .flex_none()
                    .ui(scale::FS_13)
                    .text_color(if last { p.ink } else { p.ink_3 })
                    .child(crumb.clone()),
            );
        }
        h_flex()
            .w_full()
            .flex_1()
            .items_center()
            .gap(px(scale::SP_3))
            .child(trail)
            .child(div().flex_1())
            .child(
                icon_button("settings-close", IconName::X)
                    .ghost()
                    .size(ButtonSize::Sm)
                    .accessibility_label("Close settings")
                    .on_click(move |_, window, cx| close(&(), window, cx)),
            )
            .into_any_element()
    }

    /// The settings surface: centre+right merged into one column, max 680
    /// px, centred, scrolling on its own, 32 px top padding. The surface is
    /// always the full column, so moving between sections never resizes
    /// anything. Each page: H1 + one muted line + grouped hairline boxes.
    pub(crate) fn render_settings_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Keep the filter source in one place: the search entity owns the
        // text while the page stands open.
        self.settings_page.shortcut_search =
            self.settings_search.read(cx).value().trim().to_owned();
        let section = self.settings_page.section.clone();
        let p = cx.aui().colors;
        let title: &str = if let Some(id) = provider_subpage_id(&section) {
            id.label()
        } else {
            SETTINGS_SECTIONS.iter().find(|(id, _)| *id == section).map(|(_, label)| *label).unwrap_or("General")
        };
        let blurb = settings_blurb(&section);
        let body: AnyElement = match section.as_str() {
            "general" => self.settings_general_page(cx),
            "sidebar" => self.settings_sidebar_page(cx),
            "providers" => self.settings_providers_overview(window, cx),
            "shortcuts" => self.settings_shortcuts_page(window, cx),
            "archived" => self.settings_archived_page(cx),
            _ if is_provider_subpage(&section) => self.settings_provider_subpage(window, cx),
            _ => self.settings_general_page(cx),
        };
        // While a shortcut records, the next keystroke binds it (the old
        // dialog's capture, moved onto the page): a bare modifier stays
        // armed, anything else sets the row. Escape never reaches here —
        // it matches Cancel first, which disarms through `settings_escape`.
        let capture = self.recording_shortcut.clone();
        let page = cx.entity().downgrade();
        h_flex()
            .id("settings-surface")
            .size_full()
            .justify_center()
            .key_context(crate::app::SETTINGS_CONTEXT)
            .on_action(cx.listener(|this, _: &crate::app::SettingsFind, window, cx| {
                window.focus(&this.settings_search.focus_handle(cx), cx);
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
                    .min_w(px(0.0))
                    .max_w(px(680.0))
                    .h_full()
                    .overflow_y_scroll()
                    .pt(px(32.0))
                    .px(px(scale::SP_5))
                    .pb(px(scale::SP_5))
                    .gap(px(scale::SP_4))
                    .child(
                        div()
                            .id("settings-h1")
                            .flex_none()
                            .ui(19.8)
                            .semibold()
                            .text_color(p.ink)
                            .role(gpui::Role::Heading)
                            .aria_label(format!("Settings page, {title}"))
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ui(scale::FS_13)
                            .text_color(p.ink_3)
                            .child(blurb.to_string()),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    /// One switch row: label + one detail line left, the control right.
    fn settings_switch_row(
        &self,
        row_id: &str,
        label: &str,
        detail: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = cx.aui().colors;
        let id = row_id.to_owned();
        let flip = cx.listener(move |this: &mut Self, next: &bool, _, cx| {
            this.flip_setting(&id, *next, cx);
        });
        h_flex()
            .w_full()
            .items_center()
            .gap(px(scale::SP_3))
            .py(px(scale::SP_2))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(div().text_color(p.ink).ui(scale::FS_13).child(label.to_string()))
                    .child(div().text_color(p.ink_3).ui(scale::FS_12).child(detail.to_string())),
            )
            .child(
                Switch::new(format!("settings-switch-{row_id}"))
                    .checked(on)
                    .color(p.accent)
                    .accessibility_label(SharedString::from(label.to_string()))
                    .on_click(move |next, window, cx| flip(next, window, cx)),
            )
            .into_any_element()
    }

    /// One plain hairline box with hairline dividers between rows.
    fn settings_group(&self, label: &str, rows: Vec<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let mut boxed = v_flex()
            .id(SharedString::from(format!("settings-group-{label}")))
            .w_full()
            .rounded(px(scale::R_SM))
            .border_1()
            .border_color(p.line)
            .bg(p.surface_2)
            .px(px(scale::SP_4))
            .role(gpui::Role::Group)
            .aria_label(format!("Settings group, {label}"));
        for (index, row) in rows.into_iter().enumerate() {
            if index > 0 {
                boxed = boxed.child(div().h(px(1.0)).w_full().flex_none().bg(p.line));
            }
            boxed = boxed.child(row);
        }
        v_flex()
            .w_full()
            .flex_none()
            .gap(px(scale::SP_2))
            .child(div().ui(scale::FS_12).text_color(p.ink_3).child(label.to_string()))
            .child(boxed)
            .into_any_element()
    }

    /// General: the three model-spending switches under "Model
    /// housekeeping".
    fn settings_general_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = vec![
            self.settings_switch_row(
                "auto_title",
                "Name sessions automatically",
                "Each of these spends one short call on the cheapest model. Naming runs after a session's first message.",
                self.layout.auto_title,
                cx,
            ),
            self.settings_switch_row(
                "auto_summary",
                "Summarise sessions in the sidebar",
                "Show the last request beside the last reply; rewrite poor ones.",
                self.layout.auto_summary,
                cx,
            ),
            self.settings_switch_row(
                "handoff_model_summary",
                "Summarise handoffs with a model",
                "A cheap model writes the summary the next provider reads (one short turn).",
                self.layout.handoff_model_summary,
                cx,
            ),
        ];
        v_flex().w_full().gap(px(scale::SP_4)).child(self.settings_group("Model housekeeping", rows, cx)).into_any_element()
    }

    /// Sidebar: chevron, current-project bar, branch name — plus the note
    /// that grouping and sort live in the sidebar's view menu.
    fn settings_sidebar_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let rows = vec![
            self.settings_switch_row(
                "group_chevron",
                "Collapse chevron",
                "Show a chevron on project rows to fold them.",
                self.layout.group_chevron,
                cx,
            ),
            self.settings_switch_row(
                "group_bar",
                "Current-project bar",
                "Mark the open session's project with an accent bar.",
                self.layout.group_bar,
                cx,
            ),
            self.settings_switch_row(
                "group_branch",
                "Branch name",
                "Show each project's git branch on its row.",
                self.layout.group_branch,
                cx,
            ),
        ];
        v_flex()
            .w_full()
            .gap(px(scale::SP_4))
            .child(self.settings_group("Project rows", rows, cx))
            .child(
                div()
                    .w_full()
                    .flex_none()
                    .ui(scale::FS_12)
                    .text_color(p.ink_3)
                    .child("Grouping and sort live in the sidebar's view menu.".to_string()),
            )
            .into_any_element()
    }

    /// Providers overview: one row per provider in switcher order
    /// (provider mark, name, headline, Enable switch, chevron into the
    /// sub-page) plus the footer action row.
    fn settings_providers_overview(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let statuses = crate::provider_status::live_statuses();
        let mut rows = Vec::new();
        for status in &statuses {
            let wire = status.provider.as_str().to_owned();
            let open = cx.listener({
                let wire = wire.clone();
                move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    this.open_settings_page(&format!("providers/{wire}"), cx);
                }
            });
            let provider = status.provider;
            let flip = cx.listener(move |this: &mut Self, next: &bool, _, cx| {
                this.flip_provider_enabled(provider, *next, cx);
            });
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("settings-provider-{wire}")))
                    .w_full()
                    .items_center()
                    .gap(px(scale::SP_3))
                    .py(px(scale::SP_2))
                    .child(
                        v_flex()
                            .id(SharedString::from(format!("settings-provider-open-{wire}")))
                            .flex_1()
                            .min_w(px(0.0))
                            .cursor_pointer()
                            .role(gpui::Role::Button)
                            .aria_label(format!(
                                "{} settings, {}. Open the {} page.",
                                status.provider.label(),
                                status.headline_text(),
                                status.provider.label()
                            ))
                            .on_click(open)
                            .child(
                                div()
                                    .text_color(p.ink)
                                    .ui(scale::FS_13)
                                    .child(status.provider.label().to_string()),
                            )
                            .child(
                                div()
                                    .text_color(p.ink_3)
                                    .ui(scale::FS_12)
                                    .child(status.headline_text().to_string()),
                            ),
                    )
                    .child(
                        Switch::new(format!("settings-enable-{wire}"))
                            .checked(status.enabled)
                            .color(p.accent)
                            .accessibility_label(SharedString::from(format!(
                                "Enable {}",
                                status.provider.label()
                            )))
                            .on_click(move |next, window, cx| flip(next, window, cx)),
                    )
                    .into_any_element(),
            );
        }
        let setup = cx.listener(|this: &mut Self, _: &(), _, cx| this.open_connect_screen(cx));
        let _ = window;
        v_flex()
            .w_full()
            .gap(px(scale::SP_4))
            .child(self.settings_group("Providers", rows, cx))
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
                            .ui(scale::FS_12)
                            .text_color(p.ink_3)
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

    /// One provider sub-page: the Account group (today's card) plus the
    /// Tools group ("Use my own MCP servers", Claude Code and Codex only).
    fn settings_provider_subpage(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(id) = provider_subpage_id(&self.settings_page.section) else {
            return self.settings_providers_overview(window, cx);
        };
        let statuses = crate::provider_status::live_statuses();
        let status = statuses.iter().find(|s| s.provider == id);
        let Some(status) = status else {
            return self.settings_providers_overview(window, cx);
        };
        let data = crate::settings_providers::card_data(status);
        let intent = cx.listener(move |this: &mut Self, intent: &aui::screens::ProviderIntent, window, cx| {
            this.handle_provider_intent(intent.clone(), window, cx);
        });
        let mut card_rows = vec![
            aui::screens::provider_card(format!("settings-card-{}", id.as_str()), &data)
                .on_intent(move |event, window, cx| intent(&event, window, cx))
                .into_any_element(),
        ];
        if crate::settings_providers::own_mcp_providers().contains(&id) {
            card_rows.push(self.use_own_mcp_row(id, cx));
        }
        v_flex()
            .w_full()
            .gap(px(scale::SP_4))
            .child(self.settings_group("Account", card_rows, cx))
            .into_any_element()
    }

    /// Shortcuts: a search field pinned at the top (⌘F focuses it),
    /// groups by category, non-editable rows folded into one final
    /// Reserved row, and Restore defaults only when something differs.
    fn settings_shortcuts_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let query = self.settings_search.read(cx).value().trim().to_lowercase();
        let bindings = &self.shortcuts_cache;
        let differs = shortcuts_differ_from_defaults(bindings);
        let restore = cx.listener(|this: &mut Self, _: &(), _, cx| {
            crate::keymap::restore_defaults();
            this.refresh_shortcuts();
            crate::app::bind_keys(cx);
            cx.notify();
        });
        let mut column = v_flex().w_full().gap(px(scale::SP_4));
        let mut header_row = h_flex().w_full().flex_none().items_center().gap(px(scale::SP_3));
        header_row = header_row.child(
            div()
                .id("settings-shortcut-search")
                .flex_none()
                .w(px(280.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(scale::R_SM))
                .border_1()
                .border_color(p.line)
                .bg(p.surface_1)
                .role(gpui::Role::SearchInput)
                .aria_label("Search shortcuts")
                .child(
                    gpui_kit::component::input::Textarea::new(&self.settings_search)
                        .appearance(false)
                        .bordered(false)
                        .text_size(aui_tokens::scaled(scale::FS_13))
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
            let recording = self.recording_shortcut.as_deref() == Some(id.as_str());
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
            let row = h_flex()
                .w_full()
                .items_center()
                .gap(px(scale::SP_3))
                .py(px(scale::SP_2))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(div().text_color(p.ink).ui(scale::FS_13).child(binding.label.clone()))
                        .children(detail.map(|text| {
                            div().text_color(p.ink_3).ui(scale::FS_12).child(text)
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("settings-key-{id}")))
                        .flex_none()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(scale::R_SM))
                        .border_1()
                        .border_color(p.line)
                        .bg(p.surface_1)
                        .cursor_pointer()
                        .role(gpui::Role::Button)
                        .aria_label(format!(
                            "{}, currently {}. Activate to rebind.",
                            binding.label,
                            if recording { "recording, press a key" } else { keycap.as_str() }
                        ))
                        .on_click(arm)
                        .child(
                            div()
                                .ui(scale::FS_12)
                                .text_color(p.ink)
                                .child(if recording { "press a key…".to_owned() } else { keycap }),
                        ),
                )
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
            let reserved_row = h_flex()
                .w_full()
                .items_center()
                .gap(px(scale::SP_3))
                .py(px(scale::SP_2))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .id("settings-reserved-label")
                                .text_color(p.ink)
                                .ui(scale::FS_13)
                                .role(gpui::Role::Label)
                                .aria_label("Reserved keys, terminal takes the key")
                                .child("Terminal takes the key"),
                        )
                        .child(
                            div()
                                .text_color(p.ink_3)
                                .ui(scale::FS_12)
                                .child("These keys reach the running program while the terminal is focused."),
                        ),
                )
                .child(
                    div()
                        .id("settings-reserved-keys")
                        .flex_none()
                        .ui(scale::FS_12)
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
        let mut archived: Vec<&crate::sidebar::SessionEntry> =
            self.sessions.iter().filter(|e| e.archived).collect();
        archived.sort_by(|a, b| a.updated.cmp(&b.updated).reverse());
        if archived.is_empty() {
            return v_flex()
                .w_full()
                .child(
                    div()
                        .id("settings-archived-empty")
                        .w_full()
                        .ui(scale::FS_13)
                        .text_color(p.ink_3)
                        .role(gpui::Role::Label)
                        .aria_label("No archived sessions")
                        .child("No archived sessions.".to_string()),
                )
                .into_any_element();
        }
        let mut order: Vec<String> = Vec::new();
        let mut by_project: std::collections::HashMap<String, Vec<&crate::sidebar::SessionEntry>> =
            std::collections::HashMap::new();
        for entry in archived {
            let name = entry.project_name.clone().unwrap_or_else(|| "Unfiled".to_owned());
            by_project.entry(name.clone()).or_default().push(entry);
            if !order.contains(&name) {
                order.push(name);
            }
        }
        let mut column = v_flex().w_full().gap(px(scale::SP_4));
        for name in order {
            let rows = by_project.remove(&name).unwrap_or_default();
            let mut els = Vec::new();
            for entry in rows {
                let id = entry.id.clone();
                let unarchive = cx.listener(move |this: &mut Self, _: &(), _, cx| {
                    this.unarchive_session(id.clone(), cx);
                });
                els.push(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(px(scale::SP_3))
                        .py(px(scale::SP_2))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(div().text_color(p.ink).ui(scale::FS_13).child(entry.label.clone()))
                                .child(
                                    div().text_color(p.ink_3).ui(scale::FS_12).child(
                                        entry.updated.format("%Y-%m-%d").to_string(),
                                    ),
                                ),
                        )
                        .child(
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

/// Which section `id` names: its index, or the first section when `id` is
/// empty or unknown. Pure, so tests can drive it without a window.
pub(crate) fn settings_section_index(sections: &[SettingsSection], id: &str) -> usize {
    if id.is_empty() {
        return 0;
    }
    sections.iter().position(|s| s.id.as_ref() == id).unwrap_or(0)
}

/// A Shortcuts row's stable id: the action and its context, joined on a
/// separator neither may contain, so [`parse_shortcut_row_id`] recovers both
/// for [`Harness::handle_shortcut`](crate::app::Harness::handle_shortcut).
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
/// Pure, so tests drive it without a window.
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
        assert!(apply_setting(&mut layout, "group_chevron", true));
        assert!(layout.group_chevron);
        assert!(apply_setting(&mut layout, "group_bar", true));
        assert!(layout.group_bar);
        assert!(apply_setting(&mut layout, "group_branch", true));
        assert!(layout.group_branch);
        assert!(apply_setting(&mut layout, "auto_title", false));
        assert!(!layout.auto_title);
        assert!(apply_setting(&mut layout, "auto_summary", false));
        assert!(!layout.auto_summary);
        assert!(apply_setting(&mut layout, "handoff_model_summary", false));
        assert!(!layout.handoff_model_summary);
        assert!(!apply_setting(&mut layout, "nope", true));
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
        assert_eq!(normalize_settings_target("fold_finished_turns", ""), "general");
        assert_eq!(normalize_settings_target("group_bar", ""), "sidebar");
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

    /// Every former setting is reachable on exactly one page: the three
    /// model-spending switches plus the fold switch moved to General, the
    /// three row switches stay on Sidebar, provider switches on Providers,
    /// shortcut rows on Shortcuts, archived rows on Archived.
    #[test]
    fn every_former_setting_has_exactly_one_home() {
        let cases = [
            ("auto_title", "general"),
            ("auto_summary", "general"),
            ("handoff_model_summary", "general"),
            ("fold_finished_turns", "general"),
            ("group_chevron", "sidebar"),
            ("group_bar", "sidebar"),
            ("group_branch", "sidebar"),
            ("provider-enabled:codex", "providers"),
            ("provider-enabled:claude-code", "providers"),
            ("provider-enabled:muse", "providers"),
            ("shortcut:NewSession", "shortcuts"),
            ("archived:s-1", "archived"),
        ];
        for (id, home) in cases {
            assert_eq!(setting_home(id), Some(home), "the home of {id}");
        }
        // Exactly one: no id names two pages, and unknown ids name none.
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
