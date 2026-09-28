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
use aui::overlay::popover_layer;
use aui::skills::{
    added_tokens, cost_meter, format_tokens, import_preview, menu_row_two_line,
    scope_section_header, segmented, selected_rows, skill_detail, skill_row, switch, CostSegment,
    ImportPreviewIntent, ImportRow, ImportStatus as LibraryImportStatus, InkLevel, SkillChip,
    SkillDetailIntent, SkillMode, SkillRowModel, SkillState, SwitchIntent, ChipTone, DetailAction,
};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled};
use aui::transcript::{prose, ProseStyle};
use gpui::{div, prelude::*, px, AnyElement, App, Context, Focusable, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::Textarea;

use crate::app::{
    Harness, SkillsClose, SkillsDown, SkillsEnter, SkillsFind, SkillsToggle, SkillsUp,
};
use crate::skills::{self, scope_word, Activation, ScopeSection, Skill, SkillsCatalog};
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

/// Where a New skill or an install preview lands (D61).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetScope {
    /// `<root>/.agents/skills/<name>/`.
    #[default]
    Project,
    /// A scratch folder installed with `install --scope user`.
    Personal,
}

impl TargetScope {
    /// Both scopes, in segmented order.
    pub const ALL: [TargetScope; 2] = [TargetScope::Project, TargetScope::Personal];

    /// The segmented label.
    pub fn label(&self) -> &'static str {
        match self {
            TargetScope::Project => "This project",
            TargetScope::Personal => "Personal",
        }
    }
}

/// The "Add skill ▾" menu's import trails, counted when the menu opens and
/// cached for the menu's life (A1): how many candidates per source are new.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportCounts {
    /// New Claude Code candidates.
    pub claude: usize,
    /// New Codex candidates.
    pub codex: usize,
}

/// One import-preview row: a dry-run candidate classified against the landed
/// catalog (D61).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportDialogRow {
    /// The skill name, from the source SKILL.md's frontmatter.
    pub name: String,
    /// The frontmatter description.
    pub description: String,
    /// Startup tokens, estimated from the SKILL.md bytes.
    pub tokens: u32,
    /// The candidate's SKILL.md on disk, for the per-skill `install`.
    pub source_path: String,
    /// New / Replaces yours / Already installed.
    pub status: skills::ImportStatus,
    /// Picked for import. Installed rows are never picked.
    pub checked: bool,
}

impl ImportDialogRow {
    /// Whether the preview's checkbox is live.
    pub fn selectable(&self) -> bool {
        !matches!(self.status, skills::ImportStatus::Installed)
    }

    /// The library preview row: installed rows draw unchecked and dead.
    pub fn library_row(&self) -> ImportRow {
        let status = match self.status {
            skills::ImportStatus::New => LibraryImportStatus::New,
            skills::ImportStatus::Replaces => LibraryImportStatus::Replaces,
            skills::ImportStatus::Installed => LibraryImportStatus::Installed,
        };
        let description = if self.description.trim().is_empty() {
            "No description.".to_owned()
        } else {
            self.description.clone()
        };
        let row = ImportRow::new(
            self.name.clone(),
            self.name.clone(),
            description,
            status,
            format_tokens(self.tokens),
            self.tokens,
        );
        if self.checked { row } else { row.unchecked() }
    }
}

/// The page's modal layer: one dialog at a time, above the list (A2–A6).
/// Plain data on [`SkillsPage`]; every mutation still runs `muse skills …`
/// on a background thread and re-lists (D55).
#[derive(Clone, Debug, Default)]
pub enum SkillsDialog {
    /// No dialog.
    #[default]
    Closed,
    /// New skill…: the name and description live in the page's fields
    /// ([`Harness::skills_new_name`](crate::app::Harness::skills_new_name));
    /// the scope and the inline error live here.
    New {
        /// This project or Personal.
        scope: TargetScope,
        /// The inline error under the fields, if the last Create failed.
        error: Option<String>,
    },
    /// Install from folder…: the validated preview (D61).
    InstallPreview {
        /// The picked folder.
        dir: String,
        /// The frontmatter name, else the folder name.
        name: String,
        /// The frontmatter description.
        description: String,
        /// Estimated startup tokens.
        tokens: u32,
        /// Files under the folder, relative to it.
        files: Vec<String>,
        /// The `validate` verdict, in full.
        diagnostics: String,
        /// Where Install lands.
        scope: TargetScope,
        /// The first Install press on an occupied name arms Replace; the
        /// second runs it with `--force` (D61).
        armed_replace: bool,
    },
    /// Import from Claude Code / Codex: the library preview over the
    /// dry-run (D61).
    Import {
        /// Which source the dry-run read.
        source: skills::ImportSource,
        /// Classified candidates; invalid ones never reach the rows (their
        /// count rides the subtitle instead).
        rows: Vec<ImportDialogRow>,
        /// Candidates the dry-run reported that carry no SKILL.md.
        skipped: usize,
        /// The dry-run is still in flight.
        loading: bool,
        /// The dry-run failed: the dialog shows this instead of rows.
        error: Option<String>,
    },
    /// Remove…: the confirm naming what goes (D62).
    Remove {
        /// The row's selection key.
        key: String,
        /// The skill name.
        name: String,
        /// The confirm's heading.
        headline: String,
        /// The confirm's body: the path for project rows.
        detail: String,
        /// Personal removes through `uninstall`; project rows move to the
        /// Trash. Bundled and plugin rows never open this dialog.
        personal: bool,
    },
}

/// The draft "Ask Muse to write one" prepares (A5): a new **muse** session
/// with this in the composer, focused, never sent.
pub fn create_skill_draft() -> &'static str {
    "/create-skill "
}

/// The import preview's summary note: what the picked rows add (D59, D61).
pub fn import_summary_text(rows: &[ImportRow]) -> String {
    let tokens = added_tokens(rows);
    let selected = selected_rows(rows).len();
    let noun = if selected == 1 { "skill" } else { "skills" };
    format!("Adds {} tokens · {selected} {noun}", format_tokens(tokens))
}

/// The deterministic import rows the scripted capture draws (S3): no CLI.
pub fn fixture_import_rows() -> Vec<ImportDialogRow> {
    vec![
        ImportDialogRow {
            name: "decoction".to_owned(),
            description: "Spec-anchored orchestration for long-running coding projects.".to_owned(),
            tokens: 640,
            source_path: "/tmp/k3-fixture/decoction/SKILL.md".to_owned(),
            status: skills::ImportStatus::New,
            checked: true,
        },
        ImportDialogRow {
            name: "relay".to_owned(),
            description: "Plan work, hand the implementation to Muse Code, verify it.".to_owned(),
            tokens: 1120,
            source_path: "/tmp/k3-fixture/relay/SKILL.md".to_owned(),
            status: skills::ImportStatus::Replaces,
            checked: true,
        },
        ImportDialogRow {
            name: "taste".to_owned(),
            description: "Mandatory preflight for web frontend visual design.".to_owned(),
            tokens: 470,
            source_path: "/tmp/k3-fixture/taste/SKILL.md".to_owned(),
            status: skills::ImportStatus::Installed,
            checked: false,
        },
    ]
}

/// Names taken in a New skill's target scope, for the inline collision
/// check (D61).
pub fn taken_names(catalog: &SkillsCatalog, scope: TargetScope) -> Vec<String> {
    let section = match scope {
        TargetScope::Project => ScopeSection::Project,
        TargetScope::Personal => ScopeSection::Personal,
    };
    catalog.section_rows(section).iter().map(|row| row.name.clone()).collect()
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
    /// Whether the "Add skill ▾" menu stands open.
    pub(crate) add_menu_open: bool,
    /// The menu's import trails, counted on open and cached while it lives.
    pub(crate) import_counts: Option<ImportCounts>,
    /// The menu's counts are still being dry-run.
    pub(crate) import_loading: bool,
    /// The modal layer: New, Install preview, Import preview, Remove (A2–A6).
    pub(crate) dialog: SkillsDialog,
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
            add_menu_open: false,
            import_counts: None,
            import_loading: false,
            dialog: SkillsDialog::Closed,
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

    /// Esc peels one layer at a time: a dialog, then the menu, then the
    /// page itself (D54). Selecting a session returns outright.
    pub(crate) fn close_skills(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.skills.dialog, SkillsDialog::Closed) {
            self.skills.dialog = SkillsDialog::Closed;
            cx.notify();
            return;
        }
        if self.skills.add_menu_open {
            self.skills.add_menu_open = false;
            cx.notify();
            return;
        }
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
                this.sync_menu_skills(cx);
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
                    this.sync_menu_skills(cx);
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

    /// Push the landed catalog into the `/` menu's cache for this root, so
    /// the menu follows the page after any mutation (D64).
    fn sync_menu_skills(&mut self, cx: &mut Context<Self>) {
        let root = self.skills_root();
        let rows = self.skills.catalog.rows.clone();
        self.overlays.update(cx, |overlays, _| overlays.set_skills(root, rows));
    }

    /// Toggle the "Add skill ▾" menu (A1). Opening dry-runs both sources
    /// for the import trails; the counts cache for the menu's life.
    pub(crate) fn toggle_add_menu(&mut self, cx: &mut Context<Self>) {
        if self.skills.add_menu_open {
            self.skills.add_menu_open = false;
            cx.notify();
            return;
        }
        self.skills.add_menu_open = true;
        self.skills.dialog = SkillsDialog::Closed;
        self.skills.import_counts = None;
        self.skills.import_loading = true;
        let program = self.args.program.clone();
        let catalog = self.skills.catalog.clone();
        self.wire_call(
            cx,
            move || {
                let count = |source: skills::ImportSource| {
                    skills::import_dry_run(&program, source)
                        .unwrap_or_default()
                        .iter()
                        .filter(|candidate| {
                            !candidate.source_path.is_empty()
                                && matches!(
                                    skills::import_status(&candidate.name, &candidate.source_path, &catalog),
                                    skills::ImportStatus::New
                                )
                        })
                        .count()
                };
                ImportCounts { claude: count(skills::ImportSource::ClaudeCode), codex: count(skills::ImportSource::Codex) }
            },
            |this, counts, cx| {
                this.skills.import_loading = false;
                this.skills.import_counts = Some(counts);
                cx.notify();
            },
        );
        cx.notify();
    }

    /// The menu's import trailing: "N new", "none", or "…" while counting.
    fn add_menu_trail(&self, source: skills::ImportSource) -> String {
        if self.skills.import_loading {
            return "…".to_owned();
        }
        let count = self
            .skills
            .import_counts
            .map(|counts| match source {
                skills::ImportSource::ClaudeCode => counts.claude,
                skills::ImportSource::Codex => counts.codex,
            })
            .unwrap_or(0);
        if count == 0 { "none".to_owned() } else { format!("{count} new") }
    }

    /// Open New skill… (A2): fresh fields, This project, no error.
    pub(crate) fn open_new_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.skills.add_menu_open = false;
        self.skills_new_name.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.skills_new_desc.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.skills.dialog = SkillsDialog::New { scope: TargetScope::Project, error: None };
        cx.notify();
    }

    /// Move New skill… / the install preview between scopes.
    pub(crate) fn set_target_scope(&mut self, scope: TargetScope, cx: &mut Context<Self>) {
        match &mut self.skills.dialog {
            SkillsDialog::New { scope: current, .. } => *current = scope,
            SkillsDialog::InstallPreview { scope: current, armed_replace, .. } => {
                *current = scope;
                *armed_replace = false;
            }
            _ => return,
        }
        cx.notify();
    }

    /// Close the dialog, if one stands open.
    pub(crate) fn close_skills_dialog(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.skills.dialog, SkillsDialog::Closed) {
            self.skills.dialog = SkillsDialog::Closed;
            cx.notify();
        }
    }

    /// Create the New skill (A2): validate inline, write or install, then
    /// `validate`, re-list, select the new row and open it in the editor.
    pub(crate) fn create_new_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (scope, name, description) = match &self.skills.dialog {
            SkillsDialog::New { scope, .. } => {
                (*scope, self.skills_new_name.read(cx).value().trim().to_owned(), self.skills_new_desc.read(cx).value().trim().to_owned())
            }
            _ => return,
        };
        let taken = taken_names(&self.skills.catalog, scope);
        let error = skills::validate_skill_name(&name, &taken)
            .err()
            .or_else(|| skills::validate_skill_description(&description).err());
        if let Some(error) = error {
            if let SkillsDialog::New { error: slot, .. } = &mut self.skills.dialog {
                *slot = Some(error);
            }
            cx.notify();
            return;
        }
        let root = self.skills_root();
        let program = self.args.program.clone();
        let select = name.clone();
        match scope {
            TargetScope::Project => {
                let file = match skills::create_project_skill(&root, &name, &description) {
                    Ok(file) => file,
                    Err(error) => {
                        if let SkillsDialog::New { error: slot, .. } = &mut self.skills.dialog {
                            *slot = Some(error);
                        }
                        cx.notify();
                        return;
                    }
                };
                self.skills.dialog = SkillsDialog::Closed;
                let dir = file.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| file.clone());
                self.wire_call(
                    cx,
                    move || {
                        let verdict = skills::validate_skill(&program, &dir);
                        let catalog = skills::list_catalog(&program, &root);
                        (verdict, catalog, file)
                    },
                    move |this, (verdict, catalog, file), cx| {
                        this.skills.loading = false;
                        if catalog.errors.is_empty() {
                            this.skills.catalog = catalog;
                            this.skills.error = None;
                            this.sync_menu_skills(cx);
                        } else {
                            this.skills.error = catalog.errors.first().cloned();
                        }
                        this.skills.selected = Some(select.clone());
                        this.load_selected_body(cx);
                        let (title, detail) = match verdict {
                            Ok(verdict) => ("Skill created", verdict),
                            Err(message) => ("Skill created, validate failed", message),
                        };
                        this.overlays.update(cx, |overlays, _| overlays.toast(title, detail));
                        if std::process::Command::new("open").arg(&file).spawn().is_err() {
                            crate::baaz_log!("could not open {}", file.display());
                        }
                        cx.notify();
                    },
                );
                self.skills.loading = true;
            }
            TargetScope::Personal => {
                let tmp = std::env::temp_dir().join(format!("baaz-new-skill-{name}"));
                let _ = std::fs::remove_dir_all(&tmp);
                if let Err(error) = std::fs::create_dir_all(&tmp)
                    .map_err(|e| format!("Could not create {}: {e}", tmp.display()))
                    .and_then(|_| {
                        std::fs::write(tmp.join("SKILL.md"), skills::scaffold_skill_md(&name, &description))
                            .map_err(|e| format!("Could not write the scaffold: {e}"))
                    })
                {
                    if let SkillsDialog::New { error: slot, .. } = &mut self.skills.dialog {
                        *slot = Some(error);
                    }
                    cx.notify();
                    return;
                }
                self.skills.dialog = SkillsDialog::Closed;
                self.wire_call(
                    cx,
                    move || {
                        let installed = skills::install_skill_dir(&program, &tmp, false)
                            .and_then(|_| skills::validate_skill(&program, &tmp).map(|_| ()));
                        let catalog = skills::list_catalog(&program, &root);
                        (installed, catalog)
                    },
                    move |this, (installed, catalog), cx| {
                        this.skills.loading = false;
                        match installed {
                            Ok(()) => {
                                if catalog.errors.is_empty() {
                                    this.skills.catalog = catalog;
                                    this.skills.error = None;
                                    this.sync_menu_skills(cx);
                                } else {
                                    this.skills.error = catalog.errors.first().cloned();
                                }
                                this.skills.selected = Some(select.clone());
                                this.load_selected_body(cx);
                                // The installed copy is the row's file now;
                                // open it, falling back to the scaffold.
                                let root = this.skills_root();
                                let path = this
                                    .skills
                                    .catalog
                                    .find(&select)
                                    .and_then(|row| row.disk_path(&root))
                                    .filter(|path| path.is_file());
                                if let Some(path) = path {
                                    if std::process::Command::new("open").arg(&path).spawn().is_err() {
                                        crate::baaz_log!("could not open {}", path.display());
                                    }
                                }
                                this.overlays.update(cx, |overlays, _| {
                                    overlays.toast("Skill created", format!("Personal skill `{select}` installed."));
                                });
                            }
                            Err(message) => {
                                this.skills.error = Some(message.clone());
                                this.overlays.update(cx, |overlays, _| {
                                    overlays.toast("Create failed", message);
                                });
                            }
                        }
                        cx.notify();
                    },
                );
                self.skills.loading = true;
            }
        }
        // The New dialog's fields clear only on open, so a failed Create
        // keeps what was typed.
        let _ = window;
        cx.notify();
    }

    /// Load the selected row's SKILL.md the way selection does: disk reads
    /// land synchronously, virtual skills inspect off the UI thread.
    fn load_selected_body(&mut self, cx: &mut Context<Self>) {
        let key = match self.skills.selected.clone() {
            Some(key) => key,
            None => return,
        };
        let root = self.skills_root();
        let Some(VisibleRow::Live(index)) = self.skills_lookup(&key) else { return };
        let skill = self.skills.catalog.rows[index].clone();
        if skill.is_virtual() {
            self.skills.detail_body = None;
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
        } else if let Some(body) = skills::skill_body(&self.args.program, &skill, &root) {
            self.skills.detail_body = Some((skill.id.clone(), body));
        }
    }

    /// Install from folder… (A3): the native picker, then `validate` and
    /// the preview modal.
    pub(crate) fn choose_install_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.skills.add_menu_open = false;
        cx.notify();
        cx.activate(true);
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let picked = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => {
                    crate::baaz_log!("install folder: panel cancelled");
                    return;
                }
                Ok(Err(error)) => {
                    crate::baaz_log!("install folder: panel errored: {error:#}");
                    return;
                }
                Err(_) => {
                    crate::baaz_log!("install folder: panel future dropped");
                    return;
                }
            };
            let Some(dir) = picked else { return };
            let _ = this.update(cx, |this, cx| this.preview_install_folder(dir, cx));
        }));
        let _ = window;
    }

    /// Validate a picked folder and open the install preview over it.
    fn preview_install_folder(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        let program = self.args.program.clone();
        self.wire_call(
            cx,
            move || {
                let verdict = skills::validate_skill(&program, &dir);
                let file = dir.join("SKILL.md");
                let (name, description) = if file.is_file() {
                    skills::candidate_meta(&file.to_string_lossy())
                } else {
                    (
                        dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        String::new(),
                    )
                };
                let tokens = std::fs::metadata(&file).map(|m| skills::estimate_tokens(m.len())).unwrap_or(0);
                let files = walk_skill_dir(&dir);
                (dir, name, description, tokens, files, verdict)
            },
            |this, (dir, name, description, tokens, files, verdict), cx| {
                let diagnostics = match verdict {
                    Ok(verdict) => verdict,
                    Err(message) => message,
                };
                this.skills.dialog = SkillsDialog::InstallPreview {
                    dir: dir.to_string_lossy().into_owned(),
                    name,
                    description,
                    tokens,
                    files,
                    diagnostics,
                    scope: TargetScope::Personal,
                    armed_replace: false,
                };
                cx.notify();
            },
        );
    }

    /// Install the previewed folder (A3): Personal installs through the
    /// CLI (`--force` only after the explicit Replace arm); This project
    /// copies into `.agents/skills`. Then re-list.
    pub(crate) fn confirm_install_preview(&mut self, cx: &mut Context<Self>) {
        let (dir, name, scope, armed) = match &self.skills.dialog {
            SkillsDialog::InstallPreview { dir, name, scope, armed_replace, .. } => {
                (dir.clone(), name.clone(), *scope, *armed_replace)
            }
            _ => return,
        };
        let taken = taken_names(&self.skills.catalog, scope);
        if taken.iter().any(|other| other == &name) && !armed {
            if let SkillsDialog::InstallPreview { armed_replace, .. } = &mut self.skills.dialog {
                *armed_replace = true;
            }
            cx.notify();
            return;
        }
        let root = self.skills_root();
        let program = self.args.program.clone();
        let dir_path = std::path::PathBuf::from(&dir);
        let select = name.clone();
        self.skills.dialog = SkillsDialog::Closed;
        self.wire_call(
            cx,
            move || {
                let outcome = match scope {
                    TargetScope::Personal => skills::install_skill_dir(&program, &dir_path, armed),
                    TargetScope::Project => {
                        let dest = skills::project_skill_dir(&root, &name);
                        if dest.exists() && !armed {
                            Err(format!("A skill named `{name}` already exists here."))
                        } else {
                            skills::copy_skill_dir(&dir_path, &dest)
                        }
                    }
                };
                let catalog = skills::list_catalog(&program, &root);
                (outcome, catalog)
            },
            move |this, (outcome, catalog), cx| {
                this.skills.loading = false;
                match outcome {
                    Ok(()) => {
                        if catalog.errors.is_empty() {
                            this.skills.catalog = catalog;
                            this.skills.error = None;
                            this.sync_menu_skills(cx);
                        } else {
                            this.skills.error = catalog.errors.first().cloned();
                        }
                        this.skills.selected = Some(select.clone());
                        this.load_selected_body(cx);
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Skill installed", format!("`{}` is ready.", select.as_str()));
                        });
                    }
                    Err(message) => {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Install failed", message);
                        });
                    }
                }
                cx.notify();
            },
        );
        self.skills.loading = true;
        cx.notify();
    }

    /// Open the import preview for a source (A4): the dry-run, classified
    /// against the landed catalog.
    pub(crate) fn open_import(&mut self, source: skills::ImportSource, cx: &mut Context<Self>) {
        self.skills.add_menu_open = false;
        self.skills.dialog = SkillsDialog::Import { source, rows: Vec::new(), skipped: 0, loading: true, error: None };
        let program = self.args.program.clone();
        let catalog = self.skills.catalog.clone();
        self.wire_call(
            cx,
            move || skills::import_dry_run(&program, source),
            move |this, outcome, cx| {
                match outcome {
                    Ok(candidates) => {
                        let mut rows = Vec::new();
                        let mut skipped = 0;
                        for candidate in candidates {
                            if candidate.source_path.is_empty() || !std::path::Path::new(&candidate.source_path).is_file() {
                                skipped += 1;
                                continue;
                            }
                            let status = skills::import_status(&candidate.name, &candidate.source_path, &catalog);
                            rows.push(ImportDialogRow {
                                name: candidate.name,
                                description: candidate.description,
                                tokens: candidate.tokens,
                                source_path: candidate.source_path,
                                status,
                                checked: !matches!(status, skills::ImportStatus::Installed),
                            });
                        }
                        this.skills.dialog =
                            SkillsDialog::Import { source, rows, skipped, loading: false, error: None };
                    }
                    Err(message) => {
                        this.skills.dialog =
                            SkillsDialog::Import { source, rows: Vec::new(), skipped: 0, loading: false, error: Some(message) };
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Flip one import row's checkbox. Installed rows are dead (A4).
    pub(crate) fn toggle_import_row(&mut self, name: &str, cx: &mut Context<Self>) {
        if let SkillsDialog::Import { rows, .. } = &mut self.skills.dialog {
            if let Some(row) = rows.iter_mut().find(|row| row.name == name).filter(|row| row.selectable()) {
                row.checked = !row.checked;
            }
        }
        cx.notify();
    }

    /// Run the import (A4): all selected and nothing replaced imports the
    /// source whole; otherwise each chosen skill installs on its own
    /// (`--force` only for checked "Replaces yours" rows). Then re-list
    /// and toast "Imported k skills".
    pub(crate) fn run_import(&mut self, cx: &mut Context<Self>) {
        let (source, picked) = match &self.skills.dialog {
            SkillsDialog::Import { source, rows, loading: false, error: None, .. } => {
                let picked: Vec<ImportDialogRow> =
                    rows.iter().filter(|row| row.checked && row.selectable()).cloned().collect();
                (*source, picked)
            }
            _ => return,
        };
        if picked.is_empty() {
            return;
        }
        let whole = {
            let selectable = match &self.skills.dialog {
                SkillsDialog::Import { rows, .. } => rows.iter().filter(|row| row.selectable()).count(),
                _ => 0,
            };
            picked.len() == selectable && picked.iter().all(|row| row.status == skills::ImportStatus::New)
        };
        let count = picked.len();
        let root = self.skills_root();
        let program = self.args.program.clone();
        self.skills.dialog = SkillsDialog::Closed;
        self.wire_call(
            cx,
            move || {
                let outcome = if whole {
                    skills::import_source(&program, source)
                } else {
                    let mut first_error = None;
                    for row in &picked {
                        let force = row.status == skills::ImportStatus::Replaces;
                        if let Err(error) =
                            skills::install_skill_dir(&program, std::path::Path::new(&row.source_path), force)
                        {
                            first_error = Some(error);
                            break;
                        }
                    }
                    first_error.map_or(Ok(()), Err)
                };
                let catalog = skills::list_catalog(&program, &root);
                (outcome, catalog)
            },
            move |this, (outcome, catalog), cx| {
                this.skills.loading = false;
                match outcome {
                    Ok(()) => {
                        if catalog.errors.is_empty() {
                            this.skills.catalog = catalog;
                            this.skills.error = None;
                            this.sync_menu_skills(cx);
                        } else {
                            this.skills.error = catalog.errors.first().cloned();
                        }
                        let noun = if count == 1 { "skill" } else { "skills" };
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Import done", format!("Imported {count} {noun}."));
                        });
                    }
                    Err(message) => {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Import failed", message);
                        });
                    }
                }
                cx.notify();
            },
        );
        self.skills.loading = true;
        cx.notify();
    }

    /// "Ask Muse to write one" (A5): a new session in the current project —
    /// always on the **muse** lane, whatever the remembered provider — with
    /// the `/create-skill ` draft in the composer, focused, never sent.
    pub(crate) fn ask_muse_to_write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.skills.add_menu_open = false;
        self.skills.dialog = SkillsDialog::Closed;
        self.new_provider = crate::providers::ProviderId::Muse.as_str().to_owned();
        self.pending_create_skill = true;
        self.new_session(window, cx);
    }

    /// Open Remove… for a row (A6). Bundled and plugin rows have no Remove
    /// — only Off — so they never reach this dialog.
    pub(crate) fn open_remove_dialog(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(VisibleRow::Live(index)) = self.skills_lookup(key) else { return };
        let row = self.skills.catalog.rows[index].clone();
        let personal = row.section() == ScopeSection::Personal;
        if !personal && row.section() != ScopeSection::Project {
            return;
        }
        let (headline, detail) = if personal {
            (format!("Remove {} from your personal skills?", row.name), format!("`{}` leaves every project.", row.id))
        } else {
            let root = self.skills_root();
            let path = row
                .disk_path(&root)
                .and_then(|file| file.parent().map(|dir| dir.to_path_buf()))
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_else(|| skills::project_skill_dir(&root, &row.name).to_string_lossy().into_owned());
            (format!("Remove {} from this project?", row.name), format!("Moves {path} to the Trash."))
        };
        self.skills.dialog = SkillsDialog::Remove {
            key: key.to_owned(),
            name: row.name,
            headline,
            detail,
            personal,
        };
        cx.notify();
    }

    /// Run the confirmed Remove (A6): Personal uninstalls through the CLI;
    /// project rows move the folder to the Trash, never `rm`. Then re-list.
    pub(crate) fn confirm_remove(&mut self, cx: &mut Context<Self>) {
        let (key, name, personal) = match &self.skills.dialog {
            SkillsDialog::Remove { key, name, personal, .. } => (key.clone(), name.clone(), *personal),
            _ => return,
        };
        self.skills.dialog = SkillsDialog::Closed;
        let root = self.skills_root();
        let program = self.args.program.clone();
        if personal {
            let id = self.skills.catalog.rows.iter().find(|row| row.name == name).map(|row| row.id.clone()).unwrap_or(name.clone());
            self.wire_call(
                cx,
                move || {
                    let outcome = skills::uninstall_skill(&program, &id);
                    let catalog = skills::list_catalog(&program, &root);
                    (outcome, catalog)
                },
                move |this, (outcome, catalog), cx| {
                    this.skills.loading = false;
                    match outcome {
                        Ok(()) => {
                            if catalog.errors.is_empty() {
                                this.skills.catalog = catalog;
                                this.skills.error = None;
                                this.sync_menu_skills(cx);
                            } else {
                                this.skills.error = catalog.errors.first().cloned();
                            }
                            this.overlays.update(cx, |overlays, _| {
                                overlays.toast("Skill removed", format!("`{name}` is gone."));
                            });
                        }
                        Err(message) => {
                            this.overlays.update(cx, |overlays, _| {
                                overlays.toast("Remove failed", message);
                            });
                        }
                    }
                    let _ = key;
                    cx.notify();
                },
            );
            self.skills.loading = true;
        } else {
            let dir = self
                .skills
                .catalog
                .find(&name)
                .and_then(|row| row.disk_path(&root))
                .and_then(|file| file.parent().map(|dir| dir.to_path_buf()))
                .unwrap_or_else(|| skills::project_skill_dir(&root, &name));
            match skills::move_to_trash_dir(&dir, &skills::trash_dir()) {
                Ok(_) => {
                    self.overlays.update(cx, |overlays, _| {
                        overlays.toast("Skill removed", format!("`{name}` moved to the Trash."));
                    });
                }
                Err(message) => {
                    self.overlays.update(cx, |overlays, _| {
                        overlays.toast("Remove failed", message);
                    });
                    cx.notify();
                    return;
                }
            }
            self.relist_skills(cx);
        }
        cx.notify();
    }

    /// `skills[:page|empty|add-menu|import-preview|new]`: open the page on
    /// a fixture catalog — no CLI, deterministic, offline (S3). The K3
    /// fixtures open the Add menu, the import preview and the New dialog
    /// over the page fixture, idempotently: they set the state rather than
    /// toggling it, so both settle shots agree. An unknown payload records
    /// a step failure instead of capturing a window where nothing happened.
    pub(crate) fn step_skills(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let which = rest.trim();
        if !which.is_empty()
            && which != "page"
            && which != "empty"
            && which != "add-menu"
            && which != "import-preview"
            && which != "new"
        {
            crate::steps::record_step_failure(&format!("skills:{rest}"));
            crate::baaz_log!("unknown skills fixture `{rest}`; the page fixtures open the Skills page");
            return;
        }
        let which = if which.is_empty() { "page" } else { which };
        self.skills.open = true;
        self.skills.detail_focused = false;
        self.skills.loading = false;
        self.skills.error = None;
        self.skills.add_menu_open = false;
        self.skills.dialog = SkillsDialog::Closed;
        self.skills.import_counts = None;
        self.skills.import_loading = false;
        self.skills.catalog = fixture_catalog(if which == "empty" { "empty" } else { "page" });
        // The K3 captures layer their dialog over the page fixture: the
        // menu with its cached trails, the import preview over
        // deterministic rows, the New dialog with empty fields.
        match rest.trim() {
            "add-menu" => {
                self.skills.add_menu_open = true;
                self.skills.import_counts = Some(ImportCounts { claude: 1, codex: 0 });
            }
            "import-preview" => {
                self.skills.dialog = SkillsDialog::Import {
                    source: skills::ImportSource::ClaudeCode,
                    rows: fixture_import_rows(),
                    skipped: 1,
                    loading: false,
                    error: None,
                };
            }
            "new" => {
                self.skills_new_name.update(cx, |state, cx| state.set_value(String::new(), window, cx));
                self.skills_new_desc.update(cx, |state, cx| state.set_value(String::new(), window, cx));
                self.skills.dialog = SkillsDialog::New { scope: TargetScope::Project, error: None };
            }
            _ => {}
        }
        // The capture shows the detail with the list: select the first live
        // row, with its disk body when it has one. A virtual first row
        // (the page fixture opens on a bundled skill) reads the inspect
        // fixture through the live parse, so the read-only preview draws
        // instead of "No preview available" — there is no CLI offline.
        let root = self.skills_root();
        if let Some(first) = self.skills.catalog.rows.first().cloned() {
            self.skills.selected = Some(first.id.clone());
            if !first.is_virtual() {
                if let Some(body) = skills::skill_body(&self.args.program, &first, &root) {
                    self.skills.detail_body = Some((first.id.clone(), body));
                }
            } else {
                let text = include_str!("../tests/fixtures/skills/inspect-browser-app-delivery.json");
                if let Some(body) = skills::parse_inspect_preview(text.as_bytes()) {
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

        // The header: "Skills · [project crumb ▾]", search (⌘F), "Add skill ▾".
        let open_menu = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_project_menu(this.current_project.clone(), true, cx);
        });
        let add_skill = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.toggle_add_menu(cx);
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
            .child(
                button("skills-add", "Add skill ▾")
                    .accessibility_label("Add skill")
                    .on_click(add_skill),
            );

        // The "Add skill ▾" menu never joins this column: it floats in the
        // popover layer above the page (see `render_add_menu`), so opening
        // it cannot move the meter, the filter row or the list.
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

        // `h_flex` centres on the cross axis: without `items_stretch` the
        // scroll box keeps its content height and centres in the body, so
        // its top starts inside the header whenever content and body
        // differ (Y3a) — cross sizes never flex-shrink. Stretch pins it to
        // the body's full height; the 460 px detail pane is unaffected.
        let mut body = h_flex()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .items_stretch()
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

        // The popover layer stands above the page: the Add menu, then the
        // modal layer centred on a scrim. Neither joins the column, so
        // neither moves the page's layout.
        let overlay = self.render_skills_dialog(window, cx);
        // The page fills what the centre column leaves after the wire
        // banner and the terminal dock: `flex_1` with `min_h(0)`, never
        // `size_full`. A full-height root plus the dock overflowed the
        // centre cell and pushed the dock below the fold (Y3a).
        // `overflow_hidden` keeps the absolute popover/dialog layer —
        // seated at this relative root — clipped to the page.
        let mut root = div().flex_1().min_h(px(0.0)).w_full().overflow_hidden().relative().child(column);
        if self.skills.add_menu_open {
            root = root.child(self.render_add_menu(cx));
        }
        if let Some(overlay) = overlay {
            root = root.child(overlay);
        }
        root.key_context(SKILLS_CONTEXT)
            .on_action(cx.listener(|this, _: &SkillsUp, _, cx| this.skills_move(-1, cx)))
            .on_action(cx.listener(|this, _: &SkillsDown, _, cx| this.skills_move(1, cx)))
            .on_action(cx.listener(|this, _: &SkillsToggle, _, cx| this.skills_toggle_selected(cx)))
            .on_action(cx.listener(|this, _: &SkillsEnter, _, cx| this.skills_focus_detail(cx)))
            .on_action(cx.listener(|this, _: &SkillsFind, window, cx| this.skills_focus_search(window, cx)))
            .on_action(cx.listener(|this, _: &SkillsClose, _, cx| this.close_skills(cx)))
            .into_any_element()
    }

    /// The "Add skill ▾" menu (A1): the library two-line rows in order —
    /// New skill…, Install from folder…, Import from Claude Code / Codex
    /// with their "N new" trails, Ask Muse to write one. It floats in the
    /// popover layer, right-aligned under the header — never in the page
    /// column, so opening it cannot move the page's layout. Every row
    /// carries its menu role and label from the library component.
    fn render_add_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        // One listener per row: the rows share nothing, so no handler
        // moves twice.
        let new = cx.listener(|this: &mut Self, _: &SharedString, window, cx| this.open_new_dialog(window, cx));
        let install =
            cx.listener(|this: &mut Self, _: &SharedString, window, cx| this.choose_install_folder(window, cx));
        let import_claude = cx.listener(|this: &mut Self, _: &SharedString, _, cx| {
            this.open_import(skills::ImportSource::ClaudeCode, cx);
        });
        let import_codex = cx.listener(|this: &mut Self, _: &SharedString, _, cx| {
            this.open_import(skills::ImportSource::Codex, cx);
        });
        let ask = cx.listener(|this: &mut Self, _: &SharedString, window, cx| this.ask_muse_to_write(window, cx));
        let p = cx.aui().colors;
        // A click outside the card closes the menu. The catcher is
        // transparent, so the page behind it captures unchanged.
        let dismiss = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.skills.add_menu_open = false;
            cx.notify();
        });
        let card = v_flex()
                    .id("skills-add-menu")
                    .flex_none()
                    .w(px(380.0))
                    .py(px(6.0))
                    .rounded(px(scale::R_SM))
                    .border_1()
                    .border_color(p.line)
                    .bg(p.surface_1)
                    .role(gpui::Role::Menu)
                    .aria_label("Add skill")
                    .child(
                        menu_row_two_line(
                            "skills-add-new",
                            IconName::Plus,
                            "New skill…",
                            "Scaffold a skill, here or personal",
                        )
                        .key("new")
                        .on_activate(move |key, window, cx| new(key, window, cx)),
                    )
                    .child(
                        menu_row_two_line(
                            "skills-add-install",
                            IconName::Folder,
                            "Install from folder…",
                            "Validate and preview a skill folder",
                        )
                        .key("install")
                        .on_activate(move |key, window, cx| install(key, window, cx)),
                    )
                    .child(div().flex_none().w_full().h(px(1.0)).my(px(6.0)).bg(p.line))
                    .child(
                        menu_row_two_line(
                            "skills-add-import-claude",
                            IconName::Copy,
                            "Import from Claude Code",
                            "~/.claude/skills",
                        )
                        .key("import-claude")
                        .subtitle_mono()
                        .trailing(self.add_menu_trail(skills::ImportSource::ClaudeCode))
                        .on_activate(move |key, window, cx| import_claude(key, window, cx)),
                    )
                    .child(
                        menu_row_two_line(
                            "skills-add-import-codex",
                            IconName::Copy,
                            "Import from Codex",
                            "~/.codex/skills",
                        )
                        .key("import-codex")
                        .subtitle_mono()
                        .trailing(self.add_menu_trail(skills::ImportSource::Codex))
                        .on_activate(move |key, window, cx| import_codex(key, window, cx)),
                    )
                    .child(div().flex_none().w_full().h(px(1.0)).my(px(6.0)).bg(p.line))
                    .child(
                        menu_row_two_line(
                            "skills-add-ask",
                            IconName::Sparkle,
                            "Ask Muse to write one",
                            "Drafts /create-skill in a new muse session",
                        )
                        .key("ask")
                        .trailing("uses a turn")
                        .on_activate(move |key, window, cx| ask(key, window, cx)),
                    );
        // The popover layer, never the column: the menu floats right-
        // aligned under the header and the page's layout does not move.
        popover_layer(
            div()
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("skills-add-scrim")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .role(gpui::Role::Button)
                        .aria_label("Dismiss menu")
                        .on_click(dismiss),
                )
                .child(div().absolute().top(px(52.0)).right(px(12.0)).child(card)),
        )
        .into_any_element()
    }

    /// The modal layer (A2–A6), centred over a scrim above the list.
    /// Every control carries its role and label.
    fn render_skills_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.skills.dialog.clone() {
            SkillsDialog::Closed => None,
            SkillsDialog::New { scope, error } => Some(self.render_new_dialog(window, cx, scope, error)),
            SkillsDialog::InstallPreview { dir, name, description, tokens, files, diagnostics, scope, armed_replace } => {
                Some(self.render_install_preview(cx, dir, name, description, tokens, files, diagnostics, scope, armed_replace))
            }
            SkillsDialog::Import { source, rows, skipped, loading, error } => {
                Some(self.render_import_dialog(cx, source, rows, skipped, loading, error))
            }
            SkillsDialog::Remove { headline, detail, .. } => Some(self.render_remove_dialog(cx, headline, detail)),
        }
    }

    /// The scrim-and-card shell every custom dialog draws through: a full
    /// overlay with a dim scrim and a centred 520 px card wearing the
    /// dialog role and its name.
    fn dialog_shell(&self, name: &str, card: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        div()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .child(div().absolute().top(px(0.0)).left(px(0.0)).size_full().bg(gpui::rgba(0x0000_0066)))
            .child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(
                        v_flex()
                            .id("skills-dialog-card")
                            .flex_none()
                            .w(px(520.0))
                            .p(px(16.0))
                            .gap(px(scale::SP_3))
                            .rounded(px(scale::R_SM))
                            .border_1()
                            .border_color(p.line)
                            .bg(p.surface_1)
                            .role(gpui::Role::Dialog)
                            .aria_label(name.to_owned())
                            .child(card),
                    ),
            )
            .into_any_element()
    }

    /// New skill… (A2): name and description fields with inline errors, the
    /// scope segmented, Cancel and Create.
    fn render_new_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        scope: TargetScope,
        error: Option<String>,
    ) -> AnyElement {
        let p = cx.aui().colors;
        let labels: Vec<SharedString> =
            TargetScope::ALL.iter().map(|scope| SharedString::from(scope.label())).collect();
        let active = TargetScope::ALL.iter().position(|kept| *kept == scope).unwrap_or(0);
        let pick_scope = cx.listener(|this: &mut Self, index: &usize, _, cx| {
            if let Some(scope) = TargetScope::ALL.get(*index).copied() {
                this.set_target_scope(scope, cx);
            }
        });
        let cancel = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_skills_dialog(cx));
        let create = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.create_new_skill(window, cx));
        let mut card = v_flex()
            .w_full()
            .gap(px(scale::SP_3))
            .child(div().flex_none().ui(17.6).semibold().text_color(p.ink).child("New skill"))
            .child(
                v_flex()
                    .w_full()
                    .gap(px(4.0))
                    .child(div().flex_none().ui(scale::FS_12).text_color(p.ink_3).child("Name"))
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(scale::R_SM))
                            .border_1()
                            .border_color(p.line)
                            .bg(p.surface_2)
                            .child(
                                Textarea::new(&self.skills_new_name)
                                    .appearance(false)
                                    .bordered(false)
                                    .text_size(aui_tokens::scaled(scale::FS_13))
                                    .h_auto()
                                    .whitespace_nowrap()
                                    .into_any_element(),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .ui(scale::FS_12)
                            .text_color(p.ink_3)
                            .child("Lowercase letters, digits, hyphens · up to 64."),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap(px(4.0))
                    .child(div().flex_none().ui(scale::FS_12).text_color(p.ink_3).child("Description"))
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(scale::R_SM))
                            .border_1()
                            .border_color(p.line)
                            .bg(p.surface_2)
                            .child(
                                Textarea::new(&self.skills_new_desc)
                                    .appearance(false)
                                    .bordered(false)
                                    .text_size(aui_tokens::scaled(scale::FS_13))
                                    .h_auto()
                                    .into_any_element(),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .ui(scale::FS_12)
                            .text_color(p.ink_3)
                            .child("Say what it does and when to use it."),
                    ),
            )
            .child(
                segmented("skills-new-scope", labels, active)
                    .accessibility_label("Where the skill lives")
                    .on_select(move |index, window, cx| pick_scope(&index, window, cx)),
            );
        if let Some(error) = error {
            card = card.child(div().flex_none().w_full().ui(scale::FS_13).text_color(p.warning).child(error));
        }
        card = card.child(
            h_flex()
                .flex_none()
                .w_full()
                .justify_end()
                .gap(px(scale::SP_2))
                .child(button("skills-new-cancel", "Cancel").accessibility_label("Cancel new skill").on_click(cancel))
                .child(
                    button("skills-new-create", "Create skill")
                        .accessibility_label("Create skill")
                        .on_click(create),
                ),
        );
        let _ = window;
        self.dialog_shell("New skill", card.into_any_element(), cx)
    }

    /// Install from folder…'s preview (A3): name, description, startup
    /// tokens, files, diagnostics, the scope segmented, Cancel and Install
    /// (Replace once armed on an occupied name).
    #[allow(clippy::too_many_arguments)]
    fn render_install_preview(
        &mut self,
        cx: &mut Context<Self>,
        dir: String,
        name: String,
        description: String,
        tokens: u32,
        files: Vec<String>,
        diagnostics: String,
        scope: TargetScope,
        armed_replace: bool,
    ) -> AnyElement {
        let p = cx.aui().colors;
        let labels: Vec<SharedString> =
            TargetScope::ALL.iter().map(|scope| SharedString::from(scope.label())).collect();
        let active = TargetScope::ALL.iter().position(|kept| *kept == scope).unwrap_or(0);
        let pick_scope = cx.listener(|this: &mut Self, index: &usize, _, cx| {
            if let Some(scope) = TargetScope::ALL.get(*index).copied() {
                this.set_target_scope(scope, cx);
            }
        });
        let cancel = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_skills_dialog(cx));
        let install = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.confirm_install_preview(cx));
        let file_list = if files.is_empty() { "No files found.".to_owned() } else { files.join("\n") };
        let primary = if armed_replace { "Replace" } else { "Install" };
        let hint = match scope {
            TargetScope::Project => "Copies into this project's .agents/skills.",
            TargetScope::Personal => "Installs to your personal skills.",
        };
        let mut install_button =
            button("skills-install-confirm", primary).accessibility_label(format!("{primary} skill"));
        if armed_replace {
            install_button = install_button.danger();
        }
        let card = v_flex()
            .w_full()
            .gap(px(scale::SP_3))
            .child(div().flex_none().ui(17.6).semibold().text_color(p.ink).child("Install skill"))
            .child(div().flex_none().w_full().ui(scale::FS_13).medium().text_color(p.ink).child(name))
            .child(div().flex_none().w_full().ui(scale::FS_13).text_color(p.ink_3).child(description))
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .mono(scale::FS_12)
                    .text_color(p.ink_3)
                    .child(format!("~{} tokens at startup · {}", format_tokens(tokens), dir)),
            )
            .child(
                div().flex_none().w_full().mono(scale::FS_12).text_color(p.ink_3).child(file_list),
            )
            .child(div().flex_none().w_full().ui(scale::FS_13).text_color(p.ink).child(diagnostics))
            .child(
                segmented("skills-install-scope", labels, active)
                    .accessibility_label("Where the skill lands")
                    .on_select(move |index, window, cx| pick_scope(&index, window, cx)),
            )
            .child(div().flex_none().w_full().ui(scale::FS_12).text_color(p.ink_3).child(hint))
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .justify_end()
                    .gap(px(scale::SP_2))
                    .child(
                        button("skills-install-cancel", "Cancel")
                            .accessibility_label("Cancel install")
                            .on_click(cancel),
                    )
                    .child(install_button.on_click(install)),
            );
        self.dialog_shell("Install skill", card.into_any_element(), cx)
    }

    /// Import from Claude Code / Codex (A4): the library preview over the
    /// dry-run rows, with the "Adds N tokens" note and the "Nothing is
    /// copied until you import" action row.
    fn render_import_dialog(
        &mut self,
        cx: &mut Context<Self>,
        source: skills::ImportSource,
        rows: Vec<ImportDialogRow>,
        skipped: usize,
        loading: bool,
        error: Option<String>,
    ) -> AnyElement {
        let p = cx.aui().colors;
        if loading {
            let cancel = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_skills_dialog(cx));
            let card = v_flex()
                .w_full()
                .gap(px(scale::SP_3))
                .child(
                    div()
                        .flex_none()
                        .ui(17.6)
                        .semibold()
                        .text_color(p.ink)
                        .child(format!("Import from {}", source.label())),
                )
                .child(div().flex_none().ui(scale::FS_13).text_color(p.ink_3).child("Reading skills…"))
                .child(
                    h_flex().flex_none().w_full().justify_end().child(
                        button("skills-import-cancel", "Cancel")
                            .accessibility_label("Cancel import")
                            .on_click(cancel),
                    ),
                );
            return self.dialog_shell(&format!("Import from {}", source.label()), card.into_any_element(), cx);
        }
        if let Some(error) = error {
            let cancel = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_skills_dialog(cx));
            let card = v_flex()
                .w_full()
                .gap(px(scale::SP_3))
                .child(
                    div()
                        .flex_none()
                        .ui(17.6)
                        .semibold()
                        .text_color(p.ink)
                        .child(format!("Import from {}", source.label())),
                )
                .child(div().flex_none().w_full().ui(scale::FS_13).text_color(p.warning).child(error))
                .child(
                    h_flex().flex_none().w_full().justify_end().child(
                        button("skills-import-close", "Close")
                            .accessibility_label("Close import")
                            .on_click(cancel),
                    ),
                );
            return self.dialog_shell(&format!("Import from {}", source.label()), card.into_any_element(), cx);
        }
        let library_rows: Vec<ImportRow> = rows.iter().map(ImportDialogRow::library_row).collect();
        let mut subtitle = format!("{} from {}", if rows.len() == 1 { "1 skill".to_owned() } else { format!("{} skills", rows.len()) }, source.label());
        if skipped > 0 {
            subtitle.push_str(&format!(" · {skipped} skipped"));
        }
        let summary = import_summary_text(&library_rows);
        let toggle = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
            this.toggle_import_row(id, cx);
        });
        let cancel = cx.listener(|this: &mut Self, _: &ImportPreviewIntent, _, cx| this.close_skills_dialog(cx));
        let run = cx.listener(|this: &mut Self, _: &ImportPreviewIntent, _, cx| this.run_import(cx));
        // One checkbox row per candidate (D61): each skill with its New /
        // Replaces yours / Already installed chip and tokens. The primary
        // label defaults to the picked count, so it follows the checkboxes.
        let mut preview = import_preview(format!("skills-import-{}", source.arg()), format!("Import from {}", source.label()))
            .subtitle(subtitle)
            .summary(summary)
            .hint("Nothing is copied until you import.");
        for row in library_rows {
            preview = preview.row(row);
        }
        preview
            .on_intent(move |intent, window, cx| match intent {
                ImportPreviewIntent::ToggleRow(id) => toggle(&id, window, cx),
                ImportPreviewIntent::Cancel => cancel(&intent, window, cx),
                ImportPreviewIntent::Import => run(&intent, window, cx),
            })
            .into_any_element()
    }

    /// Remove…'s confirm (A6): the heading names the skill; project rows
    /// name the folder path that moves to the Trash.
    fn render_remove_dialog(&mut self, cx: &mut Context<Self>, headline: String, detail: String) -> AnyElement {
        let p = cx.aui().colors;
        let cancel = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_skills_dialog(cx));
        let remove = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.confirm_remove(cx));
        let card = v_flex()
            .w_full()
            .gap(px(scale::SP_3))
            .child(div().flex_none().ui(17.6).semibold().text_color(p.ink).child("Remove skill"))
            .child(div().flex_none().w_full().ui(scale::FS_13).text_color(p.ink).child(headline))
            .child(div().flex_none().w_full().mono(scale::FS_12).text_color(p.ink_3).child(detail))
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .justify_end()
                    .gap(px(scale::SP_2))
                    .child(
                        button("skills-remove-cancel", "Cancel")
                            .accessibility_label("Keep skill")
                            .on_click(cancel),
                    )
                    .child(
                        button("skills-remove-confirm", "Remove")
                            .accessibility_label("Remove skill")
                            .danger()
                            .on_click(remove),
                    ),
            );
        self.dialog_shell("Remove skill", card.into_any_element(), cx)
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
        // Virtual skills read through `inspect`, which carries no body —
        // the section says whose preview it is (D60, K2 gap).
        let body_title = if skill.is_virtual() { "SKILL.md · read-only preview" } else { "SKILL.md" };
        detail = detail.section(body_title, body_element);

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
            // ⋯ → Remove… (A6): project and personal rows only. Bundled
            // and plugin rows have no Remove — only Off.
            if matches!(skill.section(), ScopeSection::Project | ScopeSection::Personal) {
                let key = skill.id.clone();
                let name = skill.name.clone();
                let remove = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    this.skills.overflow_open = false;
                    this.open_remove_dialog(&key, cx);
                });
                menu = menu.child(
                    button("skill-remove", "Remove…")
                        .accessibility_label(format!("Remove {name}"))
                        .danger()
                        .on_click(remove),
                );
            }
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

/// Files under an arbitrary folder, relative to it, capped: the install
/// preview's file list (A3). Missing folders answer empty.
fn walk_skill_dir(dir: &std::path::Path) -> Vec<String> {
    const CAP: usize = 200;
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(top) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&top) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                files.push(relative.to_string_lossy().into_owned());
                if files.len() >= CAP {
                    files.sort();
                    return files;
                }
            }
        }
    }
    files.sort();
    files
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

    #[test]
    fn taken_names_follow_the_target_scope() {
        let catalog = catalog();
        assert_eq!(taken_names(&catalog, TargetScope::Project), vec!["git", "decoction"]);
        assert_eq!(taken_names(&catalog, TargetScope::Personal), vec!["mine"]);
    }

    #[test]
    fn the_create_skill_draft_is_unsent() {
        assert_eq!(create_skill_draft(), "/create-skill ");
    }

    #[test]
    fn import_rows_classify_and_summarise() {
        let rows = fixture_import_rows();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].selectable());
        assert!(rows[1].selectable());
        assert!(!rows[2].selectable());
        let library: Vec<ImportRow> = rows.iter().map(ImportDialogRow::library_row).collect();
        assert_eq!(selected_rows(&library).len(), 2);
        assert_eq!(added_tokens(&library), 640 + 1120);
        assert_eq!(import_summary_text(&library), "Adds 1.8k tokens · 2 skills");
    }

    #[test]
    fn target_scopes_label_themselves() {
        assert_eq!(TargetScope::ALL, [TargetScope::Project, TargetScope::Personal]);
        assert_eq!(TargetScope::Project.label(), "This project");
        assert_eq!(TargetScope::Personal.label(), "Personal");
    }
}
