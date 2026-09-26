//! The Skills page (docs/15-skills.md §4, brief S2): the centre column's
//! `Route::Skills`, replacing the transcript area while it is open.
//!
//! The page reads one [`SkillsCatalog`](crate::skills::SkillsCatalog): the
//! list is the only truth (D55), every mutation runs `muse skills …` on a
//! background thread and then re-lists, and nothing moves before the re-list
//! lands. A failed CLI call sets the banner with the CLI's message and a
//! Try again row. Re-list triggers: page open, project switch, window focus,
//! and after any mutation.
//!
//! Entry points: the sidebar's "Skills" nav row (with the on-count), ⌘K
//! "Skills" ([`Command::Skills`](crate::overlays::Command)), and `/skills`
//! typed as a whole draft in the composer — intercepted, never sent. Esc or
//! selecting a session returns.
//!
//! The list, sections, meter, rows and detail pane are the K1 library
//! components (`aui::skills`): this file owns state and intents only.

use aui::data::button;
use aui::feedback::{banner, BannerActionStyle, BannerKind, BannerRun};
use aui::skills::{
    cost_meter, format_tokens, scope_section_header, segmented, skill_detail, skill_row, switch, CostSegment,
    InkLevel, SkillChip, SkillDetailIntent, SkillMode, SkillRowModel, SkillState, SwitchIntent,
    ChipTone, DetailAction,
};
use aui_tokens::{scale, ActiveAui, AuiStyled};
use aui::transcript::{prose, ProseStyle};
use gpui::{div, prelude::*, px, AnyElement, App, Context, Focusable, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::Textarea;

use crate::app::{
    Harness, SkillsClose, SkillsDown, SkillsEnter, SkillsFind, SkillsToggle, SkillsUp,
};
use crate::skills::{self, Activation, ScopeSection, Skill, SkillsCatalog};
use crate::wire::WireCall;

/// The key context the page wears, so ↑/↓/Space/↩/⌘F/Escape reach the
/// page's own actions instead of the composer's (docs/16-keymap.md).
pub(crate) const SKILLS_CONTEXT: &str = "HarnessSkills";

/// The page's filter: All plus one segment per scope section (D56).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkillsFilter {
    /// Every section.
    #[default]
    All,
    /// This project.
    Project,
    /// Personal.
    Personal,
    /// Plugins.
    Plugins,
    /// Built-in.
    Builtin,
}

impl SkillsFilter {
    /// Every filter, in segment order.
    pub const ALL: [SkillsFilter; 5] =
        [SkillsFilter::All, SkillsFilter::Project, SkillsFilter::Personal, SkillsFilter::Plugins, SkillsFilter::Builtin];

    /// The section this filter keeps, if any.
    pub fn section(&self) -> Option<ScopeSection> {
        match self {
            SkillsFilter::All => None,
            SkillsFilter::Project => Some(ScopeSection::Project),
            SkillsFilter::Personal => Some(ScopeSection::Personal),
            SkillsFilter::Plugins => Some(ScopeSection::Plugins),
            SkillsFilter::Builtin => Some(ScopeSection::Builtin),
        }
    }

    /// The segment label, with its count.
    pub fn label(&self, count: usize) -> String {
        let name = match self {
            SkillsFilter::All => "All",
            SkillsFilter::Project => "This project",
            SkillsFilter::Personal => "Personal",
            SkillsFilter::Plugins => "Plugins",
            SkillsFilter::Builtin => "Built-in",
        };
        format!("{name} · {count}")
    }
}

/// One drawn row: either a live skill or a dimmed overridden one (D58).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisibleRow {
    /// A live skill: its index into [`SkillsCatalog::rows`].
    Live(usize),
    /// A shadowed skill: its index into [`SkillsCatalog::overridden`].
    Overridden(usize),
}

/// The page's state. Plain data on [`Harness`]; the library components are
/// stateless and read it each frame.
pub struct SkillsPage {
    /// Whether the centre column shows the page instead of the transcript.
    pub(crate) open: bool,
    /// The last landed list. A failed re-list keeps the previous one.
    pub(crate) catalog: SkillsCatalog,
    /// A list or mutation round-trip is in flight.
    pub(crate) loading: bool,
    /// The banner's message, if the last CLI call failed.
    pub(crate) error: Option<String>,
    /// The active filter segment.
    pub(crate) filter: SkillsFilter,
    /// Whether `Off` rows are hidden.
    pub(crate) hide_off: bool,
    /// The selected row's skill id (live) or `overridden:<name>` (dimmed).
    pub(crate) selected: Option<String>,
    /// Whether the keyboard is in the detail pane rather than the list.
    pub(crate) detail_focused: bool,
    /// Whether Built-in shows every row rather than the first two.
    pub(crate) show_all_builtin: bool,
    /// Whether the detail's ⋯ menu is open.
    pub(crate) overflow_open: bool,
    /// Whether the window was focused on the last frame: the edge the
    /// focus re-list triggers on.
    pub(crate) window_focused: bool,
    /// The detail's SKILL.md body: the selected skill id and its stripped
    /// body. Loaded off the UI thread for virtual skills (`inspect` runs
    /// the CLI); disk reads land synchronously on selection.
    pub(crate) detail_body: Option<(String, String)>,
    /// A virtual skill's body is being inspected.
    pub(crate) detail_loading: bool,
}

impl Default for SkillsPage {
    fn default() -> Self {
        SkillsPage {
            open: false,
            catalog: SkillsCatalog::default(),
            loading: false,
            error: None,
            filter: SkillsFilter::All,
            hide_off: false,
            selected: None,
            detail_focused: false,
            show_all_builtin: false,
            overflow_open: false,
            window_focused: false,
            detail_body: None,
            detail_loading: false,
        }
    }
}

/// The loser's scope section: what "their own section" (D58) means for a
/// diagnostic scope string.
fn loser_section(loser: &crate::skills::Overridden) -> ScopeSection {
    match loser.scope.as_str() {
        "project" => ScopeSection::Project,
        "user" => ScopeSection::Personal,
        "plugin" => ScopeSection::Plugins,
        _ => ScopeSection::Builtin,
    }
}

/// A section header's mono hint: the project discovery dirs that exist
/// (D56), "all projects" for Personal, none elsewhere.
fn section_hint(section: ScopeSection, root: &str) -> Option<SharedString> {
    match section {
        ScopeSection::Project => {
            let mut present = Vec::new();
            for dir in [".agents/skills", ".claude/skills", ".codex/skills"] {
                if std::path::Path::new(root).join(dir).is_dir() {
                    present.push(dir);
                }
            }
            if present.is_empty() {
                None
            } else {
                Some(SharedString::from(present.join(" · ")))
            }
        }
        ScopeSection::Personal => Some(SharedString::from("all projects")),
        ScopeSection::Plugins | ScopeSection::Builtin => None,
    }
}

/// A scope name for the override chips: "built-in" for bundled, so the
/// winner wears "Overrides built-in git" (D58).
fn scope_word(scope: &str) -> &str {
    match scope {
        "bundled" => "built-in",
        "user" => "personal",
        "project" => "this project",
        "plugin" => "plugin",
        _ => scope,
    }
}

/// The row's startup-tokens cell: mono tokens, or "—" at zero.
fn tokens_cell(skill: &Skill) -> String {
    let tokens = skill.startup_tokens();
    if tokens == 0 {
        "—".to_owned()
    } else {
        format_tokens(tokens as u32)
    }
}

/// How many rows each filter holds: All plus one count per section.
pub fn filter_counts(catalog: &SkillsCatalog) -> [usize; 5] {
    [
        catalog.rows.len(),
        catalog.section_rows(ScopeSection::Project).len(),
        catalog.section_rows(ScopeSection::Personal).len(),
        catalog.section_rows(ScopeSection::Plugins).len(),
        catalog.section_rows(ScopeSection::Builtin).len(),
    ]
}

/// The rows the list draws, in page order: sections in D56 order, CLI order
/// within each, each section's overridden rows dimmed at its end.
pub fn visible_rows(catalog: &SkillsCatalog, filter: SkillsFilter, hide_off: bool, query: &str) -> Vec<VisibleRow> {
    let mut out = Vec::new();
    for section in ScopeSection::ALL {
        if filter.section().is_some_and(|kept| kept != section) {
            continue;
        }
        for (index, row) in catalog.rows.iter().enumerate() {
            if row.section() != section {
                continue;
            }
            if hide_off && matches!(row.activation, Activation::Off) {
                continue;
            }
            if !SkillsCatalog::matches_query(row, query) {
                continue;
            }
            out.push(VisibleRow::Live(index));
        }
        // The losers sit dimmed at the end of their own section (D58).
        for (index, loser) in catalog.overridden.iter().enumerate() {
            let loser_section = match loser.scope.as_str() {
                "project" => ScopeSection::Project,
                "user" => ScopeSection::Personal,
                "plugin" => ScopeSection::Plugins,
                _ => ScopeSection::Builtin,
            };
            if loser_section != section {
                continue;
            }
            if hide_off {
                continue;
            }
            let needle = query.trim().to_lowercase();
            if !needle.is_empty()
                && !loser.name.to_lowercase().contains(&needle)
            {
                continue;
            }
            out.push(VisibleRow::Overridden(index));
        }
    }
    out
}

/// The meter's big number and caption (D59): the sum of
/// `startup_estimated_tokens` over `on` rows, and "a of b on".
pub fn meter_text(catalog: &SkillsCatalog) -> (String, String) {
    let total = format_tokens(catalog.meter_tokens() as u32);
    let caption = format!("tokens of context · {} of {} on", catalog.on_count(), catalog.rows.len());
    (total, caption)
}

/// Whether the page's provider line shows: skills load into **muse**
/// sessions, while Claude Code and Codex read their own skill folders.
pub(crate) fn skills_provider_line(lane: crate::providers::ProviderId) -> Option<&'static str> {
    match lane {
        crate::providers::ProviderId::Muse => None,
        _ => Some("These are the skills Muse loads. Claude Code and Codex read their own skill folders."),
    }
}

/// Fixture catalogs for the scripted captures (S3): no CLI, deterministic.
/// `which` is `page` (every scope, one shadowed pair, one diagnostic) or
/// `empty` (no project skills: the empty-project state).
pub fn fixture_catalog(which: &str) -> SkillsCatalog {
    let text = match which {
        "empty" => include_str!("../tests/fixtures/skills/empty.json"),
        _ => include_str!("../tests/fixtures/skills/page.json"),
    };
    let payload: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    skills::build_catalog("/tmp/k2-fixture", payload)
}

// ---------------------------------------------------------------------------
// The window side: open, re-list, mutate, keyboard, render.
// ---------------------------------------------------------------------------

impl Harness {
    /// The workspace root the page lists for: the current project, else the
    /// launch workspace.
    pub(crate) fn skills_root(&self) -> String {
        self.current_project()
            .map(|project| project.root.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.args.workspace.to_string_lossy().into_owned())
    }

    /// Open the page: re-list on open (D55), keep the previous list behind.
    /// A list for another root is dropped first, so a project switch never
    /// shows stale rows while the new list lands.
    pub(crate) fn open_skills(&mut self, cx: &mut Context<Self>) {
        self.skills.open = true;
        self.skills.detail_focused = false;
        let root = self.skills_root();
        if self.skills.catalog.project_root != root {
            self.skills.catalog = SkillsCatalog::empty(&root);
            self.skills.selected = None;
            self.skills.detail_body = None;
        }
        self.relist_skills(cx);
        cx.notify();
    }

    /// Esc or selecting a session returns (D54).
    pub(crate) fn close_skills(&mut self, cx: &mut Context<Self>) {
        if self.skills.open {
            self.skills.open = false;
            self.skills.detail_focused = false;
            cx.notify();
        }
    }

    /// Re-list on the background executor; the previous catalog stays until
    /// the new one lands. A failed run keeps the rows and sets the banner.
    pub(crate) fn relist_skills(&mut self, cx: &mut Context<Self>) {
        if self.skills.loading {
            return;
        }
        self.skills.loading = true;
        let program = self.args.program.clone();
        let root = self.skills_root();
        self.wire_call(cx, move || skills::list_catalog(&program, &root), |this, catalog, cx| {
            this.skills.loading = false;
            if catalog.errors.is_empty() {
                // A clean list replaces the page and clears the banner; the
                // selection follows by id, dropping off the page when gone.
                let selected = this.skills.selected.clone();
                this.skills.catalog = catalog;
                this.skills.error = None;
                if selected.as_ref().is_some_and(|id| this.skills_lookup(id).is_none()) {
                    this.skills.selected = None;
                    this.skills.detail_body = None;
                }
            } else {
                this.skills.error = catalog.errors.first().cloned();
            }
            cx.notify();
        });
    }

    /// Look a selected key up: a live row, or a dimmed overridden one.
    fn skills_lookup(&self, key: &str) -> Option<VisibleRow> {
        if let Some(name) = key.strip_prefix("overridden:") {
            return self.skills.catalog.overridden.iter().position(|o| o.name == name).map(VisibleRow::Overridden);
        }
        self.skills.catalog.rows.iter().position(|s| s.id == key || s.name == key).map(VisibleRow::Live)
    }

    /// Select a row: the detail follows, and its SKILL.md starts loading.
    /// Disk reads land synchronously; virtual skills inspect off the UI
    /// thread and the pane reads "Loading…" meanwhile.
    pub(crate) fn select_skill(&mut self, key: String, cx: &mut Context<Self>) {
        self.skills.selected = Some(key.clone());
        self.skills.detail_body = None;
        self.skills.overflow_open = false;
        let root = self.skills_root();
        if let Some(VisibleRow::Live(index)) = self.skills_lookup(&key) {
            let skill = self.skills.catalog.rows[index].clone();
            if skill.is_virtual() {
                self.skills.detail_loading = true;
                let program = self.args.program.clone();
                self.wire_call(
                    cx,
                    move || skills::skill_body(&program, &skill, &root),
                    move |this, body, cx| {
                        this.skills.detail_loading = false;
                        if this.skills.selected.as_deref() == Some(key.as_str()) {
                            this.skills.detail_body = body.map(|text| (key.clone(), text));
                        }
                        cx.notify();
                    },
                );
            } else if let Some(body) = skills::skill_body(&self.args.program, &self.skills.catalog.rows[index], &root)
            {
                let id = self.skills.catalog.rows[index].id.clone();
                self.skills.detail_body = Some((id, body));
            }
        }
        cx.notify();
    }

    /// Flip one skill on or off through the CLI, then re-list: the UI moves
    /// only on the re-list, never optimistically (D55).
    pub(crate) fn toggle_skill(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(VisibleRow::Live(index)) = self.skills_lookup(key) else { return };
        let row = &self.skills.catalog.rows[index];
        let next = if row.activation.is_on() { Activation::Off } else { Activation::On };
        self.apply_skill_state(row.id.clone(), row.scope().to_owned(), next, cx);
    }

    /// Set one skill's mode through the CLI, then re-list (D55, D57).
    pub(crate) fn set_skill_mode(&mut self, key: &str, mode: SkillMode, cx: &mut Context<Self>) {
        let Some(VisibleRow::Live(index)) = self.skills_lookup(key) else { return };
        let row = &self.skills.catalog.rows[index];
        let next = match mode {
            SkillMode::Auto => Activation::On,
            SkillMode::Only => Activation::UserInvocableOnly,
        };
        self.apply_skill_state(row.id.clone(), row.scope().to_owned(), next, cx);
    }

    /// Set one skill's full state from the detail pane (D57), then re-list.
    pub(crate) fn set_skill_state(&mut self, key: &str, state: SkillState, cx: &mut Context<Self>) {
        let Some(VisibleRow::Live(index)) = self.skills_lookup(key) else { return };
        let row = &self.skills.catalog.rows[index];
        let next = match state {
            SkillState::Automatic => Activation::On,
            SkillState::OnlyMention => Activation::UserInvocableOnly,
            SkillState::Off => Activation::Off,
        };
        self.apply_skill_state(row.id.clone(), row.scope().to_owned(), next, cx);
    }

    /// One mutation round-trip: `enable|user-only|disable` through the CLI,
    /// then a re-list in the same background closure, so the page never
    /// shows a state the list did not confirm.
    fn apply_skill_state(&mut self, id: String, scope: String, activation: Activation, cx: &mut Context<Self>) {
        if self.skills.loading {
            return;
        }
        self.skills.loading = true;
        self.skills.error = None;
        let program = self.args.program.clone();
        let root = self.skills_root();
        self.wire_call(
            cx,
            move || {
                let result = skills::set_activation(&program, &root, &id, &scope, activation);
                let catalog = skills::list_catalog(&program, &root);
                (result, catalog)
            },
            |this, (result, catalog), cx| {
                this.skills.loading = false;
                if let Err(message) = result {
                    this.skills.error = Some(message);
                } else if catalog.errors.is_empty() {
                    this.skills.catalog = catalog;
                    this.skills.error = None;
                } else {
                    this.skills.error = catalog.errors.first().cloned();
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Move the list selection by `delta`, wrapping. A move from the
    /// keyboard leaves the detail pane.
    pub(crate) fn skills_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.skills_query.read(cx).value().trim().to_owned();
        let rows = visible_rows(&self.skills.catalog, self.skills.filter, self.skills.hide_off, &query);
        if rows.is_empty() {
            return;
        }
        let current = self.skills.selected.as_deref().and_then(|key| {
            rows.iter().position(|row| self.visible_key(row) == key)
        });
        let next = match current {
            Some(at) => (((at as isize + delta) % rows.len() as isize + rows.len() as isize) % rows.len() as isize) as usize,
            None => 0,
        };
        self.skills.detail_focused = false;
        let key = self.visible_key(&rows[next]);
        self.select_skill(key, cx);
    }

    /// The visible rows' selection key: the skill id, or `overridden:<name>`.
    fn visible_key(&self, row: &VisibleRow) -> String {
        match row {
            VisibleRow::Live(index) => self.skills.catalog.rows.get(*index).map(|s| s.id.clone()).unwrap_or_default(),
            VisibleRow::Overridden(index) => {
                self.skills.catalog.overridden.get(*index).map(|o| format!("overridden:{}", o.name)).unwrap_or_default()
            }
        }
    }

    /// Space on a list row: on/off (D57). Dimmed rows have no switch.
    pub(crate) fn skills_toggle_selected(&mut self, cx: &mut Context<Self>) {
        if self.skills.detail_focused {
            return;
        }
        if let Some(key) = self.skills.selected.clone() {
            self.toggle_skill(&key, cx);
        }
    }

    /// Enter on a list row: focus the detail pane.
    pub(crate) fn skills_focus_detail(&mut self, cx: &mut Context<Self>) {
        if self.skills.selected.is_some() {
            self.skills.detail_focused = true;
            cx.notify();
        }
    }

    /// ⌘F on the page: the search field takes the keyboard.
    pub(crate) fn skills_focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.skills_query.focus_handle(cx), cx);
    }

    /// `skills[:page|empty]`: open the page on a fixture catalog — no CLI,
    /// deterministic, offline (S3). An unknown payload records a step
    /// failure instead of capturing a window where nothing happened.
    pub(crate) fn step_skills(&mut self, rest: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let which = rest.trim();
        if !which.is_empty() && which != "page" && which != "empty" {
            crate::steps::record_step_failure(&format!("skills:{rest}"));
            crate::baaz_log!("unknown skills fixture `{rest}`; `page` and `empty` open the Skills page");
            return;
        }
        let which = if which.is_empty() { "page" } else { which };
        self.skills.open = true;
        self.skills.detail_focused = false;
        self.skills.loading = false;
        self.skills.error = None;
        self.skills.catalog = fixture_catalog(which);
        // The capture shows the detail with the list: select the first live
        // row, with its disk body when it has one.
        let root = self.skills_root();
        if let Some(first) = self.skills.catalog.rows.first().cloned() {
            self.skills.selected = Some(first.id.clone());
            if !first.is_virtual() {
                if let Some(body) = skills::skill_body(&self.args.program, &first, &root) {
                    self.skills.detail_body = Some((first.id.clone(), body));
                }
            }
        }
        cx.notify();
    }

    /// The lane the page speaks for: the open session's, else the pick new
    /// sessions start on.
    fn skills_lane(&self, cx: &App) -> crate::providers::ProviderId {
        self.active
            .as_ref()
            .map(|view| view.read(cx).provider_kind())
            .unwrap_or_else(|| crate::providers::ProviderId::parse(&self.new_provider))
    }

    /// The page: header, provider line, banner, meter, filter, list and
    /// detail. Every interactive element carries an accessibility role and
    /// a human label.
    pub(crate) fn render_skills_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Window focus re-lists while the page stands open (D55): edge
        // triggered, so a focused frame does no work of its own.
        let focused = window.is_window_active();
        if focused && !self.skills.window_focused {
            self.relist_skills(cx);
        }
        self.skills.window_focused = focused;

        let p = cx.aui().colors;
        let query = self.skills_query.read(cx).value().trim().to_owned();
        let rows = visible_rows(&self.skills.catalog, self.skills.filter, self.skills.hide_off, &query);
        let counts = filter_counts(&self.skills.catalog);
        let project_name =
            self.current_project().map(|project| project.name.clone()).unwrap_or_else(|| "Unfiled".to_owned());

        // The header: "Skills · [project crumb ▾]", search (⌘F), and the
        // empty slot package 2's "Add skill ▾" will fill — held open as a
        // fixed spacer so the layout does not move when it lands.
        let open_menu = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_project_menu(this.current_project.clone(), true, cx);
        });
        let header = h_flex()
            .flex_none()
            .w_full()
            .items_center()
            .gap(px(scale::SP_3))
            .px(px(12.0))
            .pt(px(16.0))
            .child(div().flex_none().ui(19.8).semibold().text_color(p.ink).child("Skills"))
            .child(div().flex_none().ui(14.3).text_color(p.ink_4).child("·"))
            .child(
                h_flex()
                    .id("skills-crumb")
                    .flex_none()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label(format!("Skills project, {project_name}. Switch project."))
                    .on_click(open_menu)
                    .child(div().ui(scale::FS_13).medium().text_color(p.ink).child(project_name))
                    .child(div().flex_none().text_color(p.ink_3).child("▾")),
            )
            .child(div().flex_1())
            .child(div().flex_none().ui(scale::FS_12).text_color(p.ink_3).child("Search"))
            .child(
                div()
                    .flex_none()
                    .w(px(220.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(scale::R_SM))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.surface_1)
                    .child(
                        Textarea::new(&self.skills_query)
                            .appearance(false)
                            .bordered(false)
                            .text_size(aui_tokens::scaled(scale::FS_13))
                            .h_auto()
                            .whitespace_nowrap()
                            .into_any_element(),
                    ),
            )
            .child(div().flex_none().w(px(120.0)));

        let mut column = v_flex().size_full().child(header);

        // Provider sessions still open the page; the line under the header
        // says whose skills these are.
        if let Some(line) = skills_provider_line(self.skills_lane(cx)) {
            column = column.child(
                div()
                    .flex_none()
                    .w_full()
                    .px(px(12.0))
                    .pt(px(6.0))
                    .ui(scale::FS_12)
                    .text_color(p.ink_3)
                    .child(line.to_owned()),
            );
        }

        // The error banner, with the CLI's message and Try again (S1).
        if let Some(error) = self.skills.error.clone() {
            let retry = cx.listener(|this: &mut Self, _, _window: &mut Window, cx| {
                this.skills.error = None;
                this.relist_skills(cx);
            });
            column = column.child(
                div().flex_none().w_full().px(px(12.0)).pt(px(8.0)).child(
                    banner("skills-error", BannerKind::Error, vec![BannerRun::Text(error.into())])
                        .action("Try again", BannerActionStyle::Secondary)
                        .on_action(move |window, cx| retry(&(), window, cx)),
                ),
            );
        }

        // The cost meter (D59).
        let (total, caption) = meter_text(&self.skills.catalog);
        let levels = [InkLevel::Ink, InkLevel::Ink2, InkLevel::Ink3, InkLevel::Ink4];
        let segments: Vec<CostSegment> = self
            .skills
            .catalog
            .meter_by_section()
            .iter()
            .zip(levels)
            .map(|((section, tokens), level)| {
                CostSegment::new(format!("{} {}", section.label(), format_tokens(*tokens as u32)), *tokens as f32, level)
            })
            .collect();
        column = column.child(div().flex_none().w_full().pt(px(8.0)).child(cost_meter(total, caption, segments)));

        // The filter row: segments with counts, the Hide off switch.
        let labels: Vec<SharedString> =
            crate::skills_page::SkillsFilter::ALL.iter().zip(counts).map(|(f, n)| SharedString::from(f.label(n))).collect();
        let active = crate::skills_page::SkillsFilter::ALL.iter().position(|f| *f == self.skills.filter).unwrap_or(0);
        let pick_filter = cx.listener(|this: &mut Self, index: &usize, _, cx| {
            if let Some(filter) = crate::skills_page::SkillsFilter::ALL.get(*index).copied() {
                this.skills.filter = filter;
                cx.notify();
            }
        });
        let flip_hide = cx.listener(|this: &mut Self, _: &SwitchIntent, _, cx| {
            this.skills.hide_off = !this.skills.hide_off;
            cx.notify();
        });
        let filter_row = h_flex()
            .flex_none()
            .w_full()
            .items_center()
            .gap(px(scale::SP_3))
            .px(px(12.0))
            .pb(px(8.0))
            .child(
                segmented("skills-filter", labels, active)
                    .accessibility_label("Filter skills by scope")
                    .on_select(move |index, window, cx| pick_filter(&index, window, cx)),
            )
            .child(div().flex_1())
            .child(div().flex_none().ui(scale::FS_12).text_color(p.ink_3).child("Hide off"))
            .child(
                switch("skills-hide-off", self.skills.hide_off, false)
                    .accessibility_label("Hide off")
                    .on_intent(move |intent, window, cx| flip_hide(&intent, window, cx)),
            );
        column = column.child(filter_row);

        // The body: the list beside the 460 px detail pane.
        let mut list = v_flex().w_full().pb(px(24.0));
        if rows.is_empty() {
            list = list.child(self.render_skills_empty(window, cx));
        } else {
            for section in ScopeSection::ALL {
                if self.skills.filter.section().is_some_and(|kept| kept != section) {
                    continue;
                }
                let live: Vec<usize> = rows
                    .iter()
                    .filter_map(|row| match row {
                        VisibleRow::Live(index) if self.skills.catalog.rows[*index].section() == section => Some(*index),
                        _ => None,
                    })
                    .collect();
                let losers: Vec<usize> = rows
                    .iter()
                    .filter_map(|row| match row {
                        VisibleRow::Overridden(index) => Some(*index),
                        _ => None,
                    })
                    .filter(|index| loser_section(&self.skills.catalog.overridden[*index]) == section)
                    .collect();
                if live.is_empty() && losers.is_empty() {
                    continue;
                }
                let hint = section_hint(section, &self.skills_root());
                let count = format!("{}", live.len() + losers.len());
                list = list.child(scope_section_header(section.label(), hint, count));
                // Built-in folds past the first two rows (D56).
                let shown: Vec<usize> = if section == ScopeSection::Builtin
                    && !self.skills.show_all_builtin
                    && live.len() > 2
                {
                    live[..2].to_vec()
                } else {
                    live.clone()
                };
                for index in shown {
                    list = list.child(self.render_skill_row(index, cx));
                }
                if section == ScopeSection::Builtin && live.len() > 2 {
                    let more = live.len() - 2;
                    if self.skills.show_all_builtin {
                        let less = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            this.skills.show_all_builtin = false;
                            cx.notify();
                        });
                        list = list.child(
                            div().w_full().px(px(12.0)).py(px(4.0)).child(
                                button("skills-builtin-less", "Show less")
                                    .accessibility_label("Show fewer built-in skills")
                                    .on_click(less),
                            ),
                        );
                    } else {
                        let show = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                            this.skills.show_all_builtin = true;
                            cx.notify();
                        });
                        list = list.child(
                            div().w_full().px(px(12.0)).py(px(4.0)).child(
                                button("skills-builtin-more", format!("Show {more} more"))
                                    .accessibility_label(format!("Show {more} more built-in skills"))
                                    .on_click(show),
                            ),
                        );
                    }
                }
                for index in losers {
                    list = list.child(self.render_overridden_row(index, cx));
                }
            }
        }

        let mut body = h_flex()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .child(
                div()
                    .id("skills-list-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .min_w(px(0.0))
                    .overflow_y_scroll()
                    .child(list),
            );
        if let Some(detail) = self.render_skill_detail(window, cx) {
            body = body.child(
                div().flex_none().w(px(460.0)).h_full().border_l_1().border_color(p.line).child(detail),
            );
        }

        column = column.child(body);

        div()
            .size_full()
            .key_context(SKILLS_CONTEXT)
            .on_action(cx.listener(|this, _: &SkillsUp, _, cx| this.skills_move(-1, cx)))
            .on_action(cx.listener(|this, _: &SkillsDown, _, cx| this.skills_move(1, cx)))
            .on_action(cx.listener(|this, _: &SkillsToggle, _, cx| this.skills_toggle_selected(cx)))
            .on_action(cx.listener(|this, _: &SkillsEnter, _, cx| this.skills_focus_detail(cx)))
            .on_action(cx.listener(|this, _: &SkillsFind, window, cx| this.skills_focus_search(window, cx)))
            .on_action(cx.listener(|this, _: &SkillsClose, _, cx| this.close_skills(cx)))
            .child(column)
            .into_any_element()
    }

    /// One live row: name, chips, description, tokens, mode chip, switch.
    fn render_skill_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let skill = self.skills.catalog.rows[index].clone();
        let mut chips = Vec::new();
        // The winner's chip names the loser it shadows (D58).
        for loser in &self.skills.catalog.overridden {
            if loser.name == skill.name && loser.by_scope == skill.scope() {
                chips.push(SkillChip::new(
                    format!("Overrides {} {}", scope_word(&loser.scope), loser.name),
                    ChipTone::Muted,
                ));
            }
        }
        if skill.section() == ScopeSection::Plugins {
            if let Some(name) = skill.path.as_deref().and_then(|path| plugin_name(path)) {
                chips.push(SkillChip::new(format!("{name} plugin"), ChipTone::Muted));
            }
        }
        if !skill.diagnostics.is_empty() {
            let count = skill.diagnostics.len();
            chips.push(SkillChip::new(
                if count == 1 { "1 issue".to_owned() } else { format!("{count} issues") },
                ChipTone::Warning,
            ));
        }
        let mode = match skill.activation {
            Activation::On => Some(SkillMode::Auto),
            Activation::UserInvocableOnly => Some(SkillMode::Only),
            Activation::Off => None,
        };
        let selected = self.skills.selected.as_deref() == Some(skill.id.as_str());
        let model = SkillRowModel {
            id: SharedString::from(skill.id.clone()),
            name: SharedString::from(skill.name.clone()),
            description: SharedString::from(skill.summary()),
            chips,
            tokens: SharedString::from(tokens_cell(&skill)),
            mode,
            on: skill.activation.is_on(),
            dimmed: false,
            selected,
        };
        let pick = cx.listener(|this: &mut Self, row: &SharedString, _, cx| {
            this.select_skill(row.to_string(), cx);
        });
        let flip = cx.listener(|this: &mut Self, row: &SharedString, _, cx| {
            this.toggle_skill(row, cx);
        });
        let set = cx.listener(|this: &mut Self, (row, mode): &(SharedString, SkillMode), _, cx| {
            this.set_skill_mode(row, *mode, cx);
        });
        skill_row(model)
            .on_select(move |row, window, cx| pick(row, window, cx))
            .on_toggle(move |row, window, cx| flip(row, window, cx))
            .on_set_mode(move |row, mode, window, cx| set(&(row.clone(), mode), window, cx))
            .into_any_element()
    }

    /// One dimmed overridden row: viewable, never flippable (D58).
    fn render_overridden_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let loser = self.skills.catalog.overridden[index].clone();
        let key = format!("overridden:{}", loser.name);
        let selected = self.skills.selected.as_deref() == Some(key.as_str());
        let model = SkillRowModel {
            id: SharedString::from(key.clone()),
            name: SharedString::from(loser.name.clone()),
            description: SharedString::from(loser.message()),
            chips: vec![SkillChip::new(loser.chip(), ChipTone::Muted)],
            tokens: SharedString::from("—"),
            mode: None,
            on: false,
            dimmed: true,
            selected,
        };
        let pick = cx.listener(move |this: &mut Self, row: &SharedString, _, cx| {
            this.select_skill(row.to_string(), cx);
        });
        skill_row(model).on_select(move |row, window, cx| pick(row, window, cx)).into_any_element()
    }

    /// The detail pane for the selection: a live skill, or a dimmed loser.
    fn render_skill_detail(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.skills.selected.clone()?;
        if let Some(name) = key.strip_prefix("overridden:") {
            return Some(self.render_overridden_detail(name, cx));
        }
        let skill = self.skills.catalog.rows.iter().find(|s| s.id == key)?.clone();
        let root = self.skills_root();
        let p = cx.aui().colors;
        let state = match skill.activation {
            Activation::On => SkillState::Automatic,
            Activation::UserInvocableOnly => SkillState::OnlyMention,
            Activation::Off => SkillState::Off,
        };
        let invoke = skill.context_cost.invoke_estimated_tokens.map(|n| format_tokens(n as u32)).unwrap_or_else(|| "—".to_owned());
        let files = skills::skill_files(&skill, &root);
        let file_count = if skill.is_virtual() { "—".to_owned() } else { format!("{}", files.len()) };
        let path = skill.path.clone().unwrap_or_default();

        let mut detail = skill_detail(format!("skill-detail-{}", skill.id), skill.name.clone())
            .scope(skill_scope_chip(&skill))
            .path(path.clone())
            .state(state)
            .stat("At startup", tokens_cell(&skill))
            .stat("When loaded", invoke)
            .stat("Last loaded", "—")
            .stat("Files", file_count)
            .hint("Applies to new sessions")
            .overflow(format!("More actions for {}", skill.name));

        // "What the model sees": the description, verbatim (D60).
        let description = skill.description.clone().unwrap_or_default();
        if !description.trim().is_empty() {
            detail = detail.section(
                "What the model sees",
                div().w_full().ui(scale::FS_13).line_height(gpui::relative(1.5)).text_color(p.ink).child(description),
            );
        }

        // SKILL.md rendered with the library markdown, frontmatter stripped.
        let body = self.skills.detail_body.clone().filter(|(id, _)| *id == skill.id).map(|(_, text)| text);
        let body_element: AnyElement = match body {
            Some(text) if !text.trim().is_empty() => prose(
                format!("skill-body-{}", skill.id),
                &text,
                ProseStyle {
                    ink: p.ink,
                    code_ink: p.ink,
                    code_bg: p.surface_2,
                    size: 13.0,
                    line_height: 1.5,
                    paragraph_gap: 8.0,
                },
            )
            .into_any_element(),
            _ if self.skills.detail_loading => {
                div().w_full().ui(scale::FS_13).text_color(p.ink_3).child("Loading…").into_any_element()
            }
            _ => div().w_full().ui(scale::FS_13).text_color(p.ink_3).child("No preview available.").into_any_element(),
        };
        detail = detail.section("SKILL.md", body_element);

        // The file list.
        if !files.is_empty() {
            detail = detail.section(
                "Files",
                div().w_full().mono(scale::FS_12).text_color(p.ink_3).child(files.join("\n")),
            );
        }

        // Diagnostics: the only coloured text (D60).
        if !skill.diagnostics.is_empty() {
            let mut diagnostics = v_flex().w_full().gap(px(4.0));
            for diagnostic in &skill.diagnostics {
                let line = diagnostic.message.clone().unwrap_or_else(|| diagnostic.code.clone());
                diagnostics = diagnostics.child(
                    div().w_full().ui(scale::FS_13).text_color(p.warning).child(format!("{}: {line}", diagnostic.code)),
                );
            }
            detail = detail.section("Diagnostics", diagnostics);
        }

        // The ⋯ menu: Validate and Copy path, with the personal line on
        // personal rows (D60, §7).
        if self.skills.overflow_open {
            let mut menu = v_flex().w_full().gap(px(4.0));
            if skill.section() == ScopeSection::Personal {
                menu = menu.child(
                    div().w_full().ui(scale::FS_12).text_color(p.ink_3).child("Personal skills apply to every project."),
                );
            }
            let disk = skill.disk_path(&root);
            if let Some(dir) = disk.as_ref().and_then(|file| file.parent().map(|d| d.to_path_buf())) {
                let program = self.args.program.clone();
                let validate = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    this.skills.overflow_open = false;
                    let program = program.clone();
                    let dir = dir.clone();
                    this.wire_call(cx, move || skills::validate_skill(&program, &dir), |this, result, cx| {
                        match result {
                            Ok(verdict) => this.overlays.update(cx, |overlays, _| {
                                overlays.toast("Skill valid", verdict);
                            }),
                            Err(message) => this.overlays.update(cx, |overlays, _| {
                                overlays.toast("Validate failed", message);
                            }),
                        }
                        cx.notify();
                    });
                    cx.notify();
                });
                menu = menu.child(
                    button("skill-validate", "Validate")
                        .accessibility_label(format!("Validate {}", skill.name))
                        .on_click(validate),
                );
            }
            let copy = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.skills.overflow_open = false;
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                cx.notify();
            });
            menu = menu.child(
                button("skill-copy-path", "Copy path").accessibility_label(format!("Copy {} path", skill.name)).on_click(copy),
            );
            detail = detail.section("More actions", menu);
        }

        // Reveal in Finder and Open in editor: real paths only (D60).
        let real = skill.disk_path(&root).filter(|path| path.is_file());
        if real.is_some() {
            detail = detail.action(DetailAction::new("reveal", "Reveal in Finder", false));
        }
        if real.is_some() {
            detail = detail.action(DetailAction::new("open", "Open in editor", true));
        }

        let key = skill.id.clone();
        let on_detail = cx.listener(move |this: &mut Self, intent: &SkillDetailIntent, _, cx| match intent {
            SkillDetailIntent::SetState(state) => this.set_skill_state(&key, *state, cx),
            SkillDetailIntent::Action(action) if action.as_ref() == "overflow" => {
                this.skills.overflow_open = !this.skills.overflow_open;
                cx.notify();
            }
            SkillDetailIntent::Action(action) if action.as_ref() == "reveal" => {
                if let Some(row) = this.skills.catalog.find(&key) {
                    if let Some(path) = row.disk_path(&this.skills_root()) {
                        cx.reveal_path(&path);
                    }
                }
            }
            SkillDetailIntent::Action(action) if action.as_ref() == "open" => {
                if let Some(row) = this.skills.catalog.find(&key) {
                    if let Some(path) = row.disk_path(&this.skills_root()) {
                        if std::process::Command::new("open").arg(&path).spawn().is_err() {
                            crate::baaz_log!("could not open {}", path.display());
                        }
                    }
                }
            }
            SkillDetailIntent::Action(_) => {}
        });
        Some(detail.on_intent(move |intent, window, cx| on_detail(&intent, window, cx)).into_any_element())
    }

    /// A dimmed loser's detail: name, scope, path and why it lost (D58).
    fn render_overridden_detail(&self, name: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let loser = self.skills.catalog.overridden.iter().find(|o| o.name == name).cloned();
        let (scope, path, message) = match loser {
            Some(loser) => (scope_word(&loser.scope).to_owned(), loser.path.clone().unwrap_or_default(), loser.message()),
            None => ("—".to_owned(), String::new(), "Shadowed by a higher-priority skill.".to_owned()),
        };
        skill_detail(format!("skill-detail-overridden-{name}"), name.to_owned())
            .scope(scope)
            .path(path)
            .state(SkillState::Off)
            .hint("Applies to new sessions")
            .section(
                "Status",
                div().w_full().ui(scale::FS_13).text_color(p.ink).child(message),
            )
            .into_any_element()
    }

    /// The empty list: the search miss, or the empty-project state (D56).
    fn render_skills_empty(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let query = self.skills_query.read(cx).value().trim().to_owned();
        let copy = if !query.is_empty() {
            format!("No skills match “{query}”.")
        } else {
            "No skills in this project yet. Skills in .agents/skills travel with the repo.".to_owned()
        };
        v_flex()
            .w_full()
            .items_center()
            .justify_center()
            .gap(px(scale::SP_3))
            .py(px(48.0))
            .child(div().ui(14.3).medium().text_color(p.ink_2).child("Nothing here"))
            .child(div().ui(scale::FS_13).text_color(p.ink_3).child(copy))
            .into_any_element()
    }
}

/// The plugin name out of a `plugin://<name>/…` virtual path.
fn plugin_name(path: &str) -> Option<&str> {
    path.strip_prefix("plugin://")?.split('/').next().filter(|name| !name.is_empty())
}

/// The scope chip the detail wears.
fn skill_scope_chip(skill: &Skill) -> String {
    match skill.section() {
        ScopeSection::Project => "This project".to_owned(),
        ScopeSection::Personal => "Personal".to_owned(),
        ScopeSection::Builtin => "Built-in".to_owned(),
        ScopeSection::Plugins => match skill.path.as_deref().and_then(plugin_name) {
            Some(name) => format!("Plugin · {name}"),
            None => "Plugin".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> SkillsCatalog {
        skills::build_catalog(
            "/root",
            serde_json::json!({
                "skills": [
                    {"id": "git", "name": "git", "scope": "project", "activation": "on",
                     "context_cost": {"startup_estimated_tokens": 100}},
                    {"id": "decoction", "name": "decoction", "scope": "project", "activation": "on",
                     "context_cost": {"startup_estimated_tokens": 50}},
                    {"id": "mine", "name": "mine", "scope": "user", "activation": "off",
                     "context_cost": {"startup_estimated_tokens": 200}},
                    {"id": "bundled:plan", "name": "plan", "scope": "bundled", "activation": "on",
                     "context_cost": {"startup_estimated_tokens": 300}}
                ],
                "diagnostics": [
                    {"code": "skill-shadowed",
                     "message": "skill `decoction` skipped because a higher-priority source defines the same id",
                     "scope": "user", "path": "$HOME/.claude/skills/decoction/SKILL.md"}
                ]
            }),
        )
    }

    #[test]
    fn filters_count_every_section() {
        let counts = filter_counts(&catalog());
        assert_eq!(counts, [4, 2, 1, 0, 1]);
    }

    #[test]
    fn visible_rows_group_losers_with_their_section() {
        let rows = visible_rows(&catalog(), SkillsFilter::All, false, "");
        // Project's two live rows, then Personal's off row and its loser,
        // then Built-in's row.
        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[3], VisibleRow::Overridden(_)));
        let personal = visible_rows(&catalog(), SkillsFilter::Personal, false, "");
        assert_eq!(personal.len(), 2);
    }

    #[test]
    fn hide_off_drops_off_rows_and_losers() {
        let rows = visible_rows(&catalog(), SkillsFilter::All, true, "");
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| matches!(row, VisibleRow::Live(_))));
    }

    #[test]
    fn search_reaches_descriptions_and_loser_names() {
        assert_eq!(visible_rows(&catalog(), SkillsFilter::All, false, "git").len(), 1);
        assert_eq!(visible_rows(&catalog(), SkillsFilter::All, false, "decoction").len(), 2);
        assert!(visible_rows(&catalog(), SkillsFilter::All, false, "zzz").is_empty());
    }

    #[test]
    fn the_winner_names_its_loser() {
        let catalog = catalog();
        let loser = &catalog.overridden[0];
        assert_eq!(loser.by_scope, "project");
        assert_eq!(loser.chip(), "Overridden by this project's decoction");
    }

    #[test]
    fn fixtures_load_without_a_cli() {
        let page = fixture_catalog("page");
        assert_eq!(page.rows.len(), 23);
        assert_eq!(page.overridden.len(), 1);
        assert!(page.errors.is_empty());
        let empty = fixture_catalog("empty");
        assert!(empty.section_rows(ScopeSection::Project).is_empty());
    }

    #[test]
    fn provider_lanes_get_the_line() {
        use crate::providers::ProviderId;
        assert_eq!(skills_provider_line(ProviderId::Muse), None);
        assert_eq!(
            skills_provider_line(ProviderId::ClaudeCode),
            Some("These are the skills Muse loads. Claude Code and Codex read their own skill folders.")
        );
        assert!(skills_provider_line(ProviderId::Codex).is_some());
    }

    #[test]
    fn plugin_paths_name_their_plugin() {
        assert_eq!(plugin_name("plugin://threejs/skills/threejs/SKILL.md"), Some("threejs"));
        assert_eq!(plugin_name("bundled://x/SKILL.md"), None);
    }
}
