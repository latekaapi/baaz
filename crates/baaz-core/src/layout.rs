//! The window's own preferences: the sidebar width, how the session list
//! groups, which groups stand closed, and how far search looks.
//!
//! Global, not per workspace: the divider sits in the same place whatever the
//! window opened, the way the traffic-lights rail does, and so does the
//! grouping — these are window preferences, not project ones. The file is
//! `~/Library/Application Support/baaz/layout.json`, written atomically
//! through [`crate::store`], and every read is best-effort: a missing or
//! unparseable file is the defaults, which lose a preference and never
//! a session.
//!
//! The model only (docs/82 4.11): the live copy as a gpui global, the
//! library's theme/density/text-size tokens and the drag math, which clamps
//! into the library's shell range, are the app's `layout` module.

use std::path::PathBuf;

use serde::{de, Deserialize, Serialize, Serializer};

/// How the sidebar groups its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// Flat rows under calendar-day headers, as the window always did.
    Date,
    /// One collapsible group per project, then the unfiled group.
    Project,
}

/// Settings › Appearance › Theme (docs/83 §7, task 2.9): follow macOS, or
/// keep Baaz light or dark. Dark when nothing is stored — Baaz has always
/// opened dark, and a deterministic capture reads the default, so a capture
/// never depends on the machine's appearance (docs/83 §9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    /// Follow the system's light/dark appearance, and its changes.
    System,
    /// Always light.
    Light,
    /// Always dark.
    #[default]
    Dark,
}

impl ThemePref {
    /// Every choice, in the segmented control's order.
    pub const ALL: [ThemePref; 3] = [ThemePref::System, ThemePref::Light, ThemePref::Dark];

    /// The control's word for the choice.
    pub fn label(self) -> &'static str {
        match self {
            ThemePref::System => "System",
            ThemePref::Light => "Light",
            ThemePref::Dark => "Dark",
        }
    }

    #[cfg(feature = "probe")]
    /// `system` / `light` / `dark` (case-insensitive), as the store and the
    /// `settings-set:theme=` step spell them.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "system" => Some(ThemePref::System),
            "light" => Some(ThemePref::Light),
            "dark" => Some(ThemePref::Dark),
            _ => None,
        }
    }

}

/// Settings › Appearance › Density (task 2.10, docs/83 §2.4–2.5):
/// Comfortable is the design's type and 32 px rows; Compact is the type
/// and 28 px rows Baaz drew before 2.10. Comfortable when nothing is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DensityPref {
    /// The design's values.
    #[default]
    Comfortable,
    /// The pre-2.10 type, rows 28.
    Compact,
}

impl DensityPref {
    /// Every choice, in the segmented control's order.
    pub const ALL: [DensityPref; 2] = [DensityPref::Comfortable, DensityPref::Compact];

    #[cfg(feature = "probe")]
    /// `comfortable` / `compact` (case-insensitive), as the store and the
    /// `settings-set:density=` step spell them.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "comfortable" => Some(DensityPref::Comfortable),
            "compact" => Some(DensityPref::Compact),
            _ => None,
        }
    }
}

/// Settings › Appearance › Text size (task 2.10): Large moves every text
/// role one step up the type ladder (`aui_tokens::TextRole::larger`).
/// Default when nothing is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSizePref {
    /// The ladder as designed.
    #[default]
    Default,
    /// One step up the ladder.
    Large,
}

impl TextSizePref {
    /// Every choice, in the segmented control's order.
    pub const ALL: [TextSizePref; 2] = [TextSizePref::Default, TextSizePref::Large];

    #[cfg(feature = "probe")]
    /// `default` / `large` (case-insensitive), as the store and the
    /// `settings-set:text-size=` step spell them.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "default" => Some(TextSizePref::Default),
            "large" => Some(TextSizePref::Large),
            _ => None,
        }
    }
}

/// How sessions are ordered inside their group (the view menu's Sort by).
/// Pinned rows stay first whatever the order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    /// The latest activity first.
    #[default]
    Recent,
    /// The newest session first.
    Created,
    /// Alphabetical by title.
    Name,
}

/// Which right-pane kind was last shown, so reopening restores it. Task
/// T3's ⌘K rows reuse [`RightKind::label`]; task T4's `right:<kind>` step
/// verb parses [`RightKind::slug`].
///
/// B8 merged the old Diff review and Git changes into one [`RightKind::Changes`]
/// view (this session's edits on top, the rest of the working tree below).
/// A stored pre-merge `diff`/`git` still loads, as `Changes`
/// ([`RightKind::parse`]), and writes back as `changes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightKind {
    /// An embedded browser.
    Browser,
    /// This session's edits over the rest of the working tree.
    Changes,
    /// The workspace file tree.
    Files,
    /// The project's terminal tabs, when the terminal sits in the pane
    /// ([`TerminalPlace::Pane`], docs/82 8.1).
    Terminal,
    /// What the session's agent started in Baaz's terminal (docs/82 6.5).
    Tasks,
}

impl Serialize for RightKind {
    /// The [`RightKind::slug`]: the legacy aliases write as `changes`, so a
    /// stored `diff`/`git` migrates the moment anything re-saves the file.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.slug())
    }
}

impl<'de> Deserialize<'de> for RightKind {
    /// The inverse of [`RightKind::slug`], through [`RightKind::parse`]: the
    /// pre-merge `diff`/`git` slugs load as `Changes`, like `changes` does.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        RightKind::parse(&text).ok_or_else(|| {
            de::Error::unknown_variant(text.as_str(), &["browser", "changes", "files", "terminal", "tasks", "diff", "git"])
        })
    }
}

impl Default for RightKind {
    /// Files: what the pane shows when nothing was ever stored.
    fn default() -> Self {
        RightKind::Files
    }
}

impl RightKind {
    /// Every kind the pane offers, in strip order: Changes · Files · Page ·
    /// Terminal · Tasks. The tab row always carries the first four; Tasks
    /// rides it while it shows or while the session has a task (the app
    /// decides).
    pub const ALL: [RightKind; 5] =
        [RightKind::Changes, RightKind::Files, RightKind::Browser, RightKind::Terminal, RightKind::Tasks];

    /// The kinds a second pane may show below the first (docs/82 8.1), in
    /// the views menu's order. The Page is top-only: its native view is
    /// placed where the strip puts it, and only the strip slides.
    pub const BELOW: [RightKind; 4] = [RightKind::Changes, RightKind::Files, RightKind::Terminal, RightKind::Tasks];

    /// Whether this kind may sit in the lower pane ([`Self::BELOW`]).
    pub fn sits_below(self) -> bool {
        Self::BELOW.contains(&self)
    }

    /// The user-visible name: the tab row's labels, and T3's ⌘K labels.
    /// The legacy aliases label as the view they load into.
    pub fn label(self) -> &'static str {
        match self {
            // D6: the session's one page (docs/43 §2.7); `browser` stays the slug.
            RightKind::Browser => "Page",
            RightKind::Changes => "Changes",
            RightKind::Files => "Files",
            RightKind::Terminal => "Terminal",
            RightKind::Tasks => "Tasks",
        }
    }

    /// The machine name: the tab row's ids, and T4's `right:<kind>` verb
    /// parses these back. The legacy aliases slug as the kind they load
    /// into, so a stored `diff`/`git` round-trips through `changes`.
    pub fn slug(self) -> &'static str {
        match self {
            RightKind::Browser => "browser",
            RightKind::Changes => "changes",
            RightKind::Files => "files",
            RightKind::Terminal => "terminal",
            RightKind::Tasks => "tasks",
        }
    }

    /// The inverse of [`RightKind::slug`]: `None` for an unknown string.
    /// The pre-merge slugs `diff` and `git` load as `Changes`, so persisted
    /// layouts and step scripts written before B8 keep opening the merged
    /// view instead of failing. (Stored JSON deserialises straight to
    /// `Changes` too — no read ever produces the legacy variants — and
    /// [`right_kind`] normalises an in-memory legacy value on read.)
    pub fn parse(s: &str) -> Option<RightKind> {
        match s {
            "browser" | "page" => Some(RightKind::Browser),
            "changes" | "diff" | "git" => Some(RightKind::Changes),
            "files" => Some(RightKind::Files),
            "terminal" => Some(RightKind::Terminal),
            "tasks" => Some(RightKind::Tasks),
            _ => None,
        }
    }
}

/// Where the terminal sits (docs/82 8.1): docked under the composer in the
/// centre column, or in the right pane as its Terminal view. One terminal
/// either way — the same tabs and the same ptys; only the place it draws
/// changes. Bottom when nothing is stored, so every `layout.json` written
/// before 8.1 reads as the dock it always was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalPlace {
    /// The dock under the composer (D42).
    #[default]
    Bottom,
    /// The right pane's Terminal view, on top or in the lower pane.
    Pane,
}

impl TerminalPlace {
    #[cfg(feature = "probe")]
    /// `bottom` / `pane`, as the store and the `terminal-place:` step spell
    /// them.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "bottom" => Some(TerminalPlace::Bottom),
            "pane" => Some(TerminalPlace::Pane),
            _ => None,
        }
    }
}

/// The lower pane's divider (docs/82 8.1): the top pane's share of the
/// pane's height, held to [`SPLIT_MIN`]..=[`SPLIT_MAX`].
pub const SPLIT_DEFAULT: f32 = 0.5;
/// The smallest share the top pane keeps.
pub const SPLIT_MIN: f32 = 0.25;
/// The largest share the top pane takes.
pub const SPLIT_MAX: f32 = 0.75;

/// A divider share held into [`SPLIT_MIN`]..=[`SPLIT_MAX`]; anything that is
/// not a number reads as the default.
pub fn clamp_split(share: f32) -> f32 {
    if share.is_finite() { share.clamp(SPLIT_MIN, SPLIT_MAX) } else { SPLIT_DEFAULT }
}

/// Which half of the window the person is in (B2a-H2): today's sessions
/// column and transcript, or the person's own tabs with a tab's page in
/// the centre. The window, the project switcher, the switch itself and
/// the account footer stay put across both; everything between the
/// switch and the footer slides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Today's Baaz: the sessions sidebar and the open session.
    #[default]
    Sessions,
    /// The person's tabs: the tab list in the sidebar, a tab's page in
    /// the centre, the right pane closed unless the person opened it here.
    Browse,
}

impl Mode {
    #[cfg(feature = "probe")]
    /// The machine name: what `layout.json` stores and `mode:` parses.
    pub fn slug(self) -> &'static str {
        match self {
            Mode::Sessions => "sessions",
            Mode::Browse => "browse",
        }
    }

    /// The inverse of [`Mode::slug`]: `None` for an unknown string, so an
    /// old or hand-edited file reads as today's behaviour, never an error.
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "sessions" => Some(Mode::Sessions),
            "browse" => Some(Mode::Browse),
            _ => None,
        }
    }

    /// The other half of the window: what ⌥⌘B toggles to.
    pub fn other(self) -> Mode {
        match self {
            Mode::Sessions => Mode::Browse,
            Mode::Browse => Mode::Sessions,
        }
    }
}

impl<'de> Deserialize<'de> for Mode {
    /// What [`Mode::parse`] says, falling back to sessions: a stored
    /// mode from a newer build — or a hand-edited file — never fails
    /// the whole read, it just opens today's Baaz. Anything that is not
    /// a known mode string (`null`, a number, an object, an unknown
    /// slug) reads as sessions, so one bad value never fails the file.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value.as_str().and_then(Mode::parse).unwrap_or(Mode::Sessions))
    }
}

/// The Sessions right pane, set aside while Browse has the window: Browse
/// stands with its own pane closed, and this is what Sessions gets back.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionsPane {
    /// Whether the pane stood open.
    #[serde(default)]
    pub open: bool,
    /// Which kind it showed, if it was ever opened.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::store::lenient")]
    pub kind: Option<RightKind>,
    /// The lower pane's kind, if one stood open (docs/82 8.1).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::store::lenient")]
    pub lower: Option<RightKind>,
    /// The divider's share, if it was ever dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<f32>,
}

/// Whether provider sessions may inherit the owner's own MCP servers
/// and connectors (Z5). Off means inherit-nothing: Codex sessions carry
/// the full-table disables (plus plugin disables) and Claude Code
/// sessions carry `--strict-mcp-config`. On restores exactly the argv
/// without Baaz's scoping (the bridge still rides when a relay is set).
/// Applies to sessions started after the change. Both off by default, so
/// every `layout.json` written before this feature reads as today's
/// behaviour with no migration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseOwnMcp {
    /// Codex: inherit `~/.codex/config.toml` servers and plugins too.
    #[serde(default)]
    pub codex: bool,
    /// Claude Code: inherit the owner's MCP servers and connectors too.
    #[serde(rename = "claudeCode", default)]
    pub claude_code: bool,
}

impl UseOwnMcp {
    /// Whether `id`'s sessions inherit the owner's own servers: the switch
    /// for that provider, off for one that has none.
    pub fn inherits(&self, id: crate::providers::ProviderId) -> bool {
        match id {
            crate::providers::ProviderId::ClaudeCode => self.claude_code,
            crate::providers::ProviderId::Codex => self.codex,
            _ => false,
        }
    }

    /// Set `id`'s switch; a provider with no switch has nothing to set.
    pub fn set(&mut self, id: crate::providers::ProviderId, on: bool) {
        match id {
            crate::providers::ProviderId::ClaudeCode => self.claude_code = on,
            crate::providers::ProviderId::Codex => self.codex = on,
            _ => {}
        }
    }
}

/// What the person asked to hear about (Settings › Notifications, task
/// 3.5). Every event on, the badge on, the sound off by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyPrefs {
    /// A session moved to needing the person (approval or question).
    pub needs_you: bool,
    /// A turn finished while nobody was reading it.
    pub done: bool,
    /// A turn ended in an error.
    pub failed: bool,
    /// Play the system sound with each notification.
    pub sound: bool,
    /// Show the needs-you count on the Dock icon.
    pub badge: bool,
}

impl Default for NotifyPrefs {
    fn default() -> Self {
        Self { needs_you: true, done: true, failed: true, sound: false, badge: true }
    }
}

/// Settings › Notifications, as stored (task 3.5). Every field defaults the
/// way [`NotifyPrefs::default`] does, so a partial
/// record reads the rest as defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotifySettings {
    /// A session needs you (an approval or a question).
    pub needs_you: bool,
    /// A turn finished while you were elsewhere.
    pub done: bool,
    /// A turn ended in an error.
    pub failed: bool,
    /// Play a sound with each notification.
    pub sound: bool,
    /// Count the sessions that need you on the Dock icon.
    pub badge: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        let d = NotifyPrefs::default();
        Self { needs_you: d.needs_you, done: d.done, failed: d.failed, sound: d.sound, badge: d.badge }
    }
}

impl NotifySettings {
    fn prefs(self) -> NotifyPrefs {
        NotifyPrefs { needs_you: self.needs_you, done: self.done, failed: self.failed, sound: self.sound, badge: self.badge }
    }
}

/// What `layout.json` holds. `None` is "never resized": the default width.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    /// The settled sidebar width in window pixels, if the person ever set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
    /// The explicit grouping choice. `None` is "undecided": the window reads
    /// Project (docs/83 §4) and persists the choice the first time the
    /// person toggles it.
    #[serde(rename = "groupBy", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::store::lenient")]
    pub group_by: Option<GroupBy>,
    /// Group ids standing closed: project ids, and `"other"` for the
    /// unfiled group. Every group reads it the same way, and every group
    /// starts open.
    #[serde(rename = "closedGroups", default, skip_serializing_if = "Vec::is_empty")]
    pub closed_groups: Vec<String>,
    /// Project-group ids whose held-back rows stand shown: the "Show N more"
    /// row's flip side. Persisted exactly like
    /// `closed_groups`, and empty by default so old files read unchanged.
    #[serde(rename = "expandedGroups", default, skip_serializing_if = "Vec::is_empty")]
    pub expanded_groups: Vec<String>,
    /// Whether the search palette looks across every project (`true`) or
    /// only the current one. Stored here from
    /// the start so the toggle has somewhere to land.
    #[serde(rename = "searchAllProjects", default = "default_search_all")]
    pub search_all_projects: bool,
    /// The session order inside each group (Sort by).
    #[serde(rename = "sortBy", default, deserialize_with = "crate::store::lenient")]
    pub sort_by: SortBy,
    /// Retained for old `layout.json` files and the `group-bar` step verb:
    /// no longer gates anything. The current project's 2 px accent mark
    /// draws from `current(true)` alone (agentic-ui v0.3.15), so this flag
    /// is accepted and ignored — Settings no longer offers it.
    #[serde(rename = "groupBar", default)]
    pub group_bar: bool,
    /// Settings › Appearance › Theme: see [`ThemePref`]. An unknown value
    /// reads as the default rather than failing the file.
    #[serde(rename = "theme", default, deserialize_with = "crate::store::lenient")]
    pub theme: ThemePref,
    /// Settings › Appearance › Density: see [`DensityPref`].
    #[serde(rename = "density", default, deserialize_with = "crate::store::lenient")]
    pub density: DensityPref,
    /// Settings › Appearance › Text size: see [`TextSizePref`].
    #[serde(rename = "textSize", default, deserialize_with = "crate::store::lenient")]
    pub text_size: TextSizePref,
    /// Settings › Appearance › "Show a summary line" (docs/83 §4): every
    /// session row takes a second line under its title (52 px) — the status
    /// sentence, else the byline. OFF by default: rows are one line (32).
    /// Replaces the old `compactRows` key (whose default was the two-line
    /// row); that key is no longer read, so every install — the ones that
    /// never touched it included, which were indistinguishable on disk
    /// from a deliberate "comfortable" — starts on the new one-line default
    /// (docs/83 §9, task 2.5).
    #[serde(rename = "summaryLine", default)]
    pub summary_line: bool,
    /// Settings › Notifications (task 3.5). `None` is the default, so an
    /// old `layout.json` reads every event on, the badge on, no sound.
    #[serde(rename = "notify", default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<NotifySettings>,
    /// Whether project group rows trail the workspace branch in mono. Off
    /// by default (O4). Settings › Sidebar owns the switch; `--steps
    /// group-branch` flips it for captures.
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
    /// Whether a handoff checkpoint spends one cheap model call on a
    /// model-written pack summary (Z8). ON by default; off means the pack
    /// keeps the extractive summary (the first lines of the earliest
    /// replies) and no summary side session ever starts. The Settings
    /// dialog's Sidebar section owns the switch, like [`Self::auto_title`].
    #[serde(rename = "handoffModelSummary", default = "default_true")]
    pub handoff_model_summary: bool,
    /// Whether a settled assistant turn folds its work behind one header
    /// row while the final answer stays out (B12). ON by default; off
    /// leaves every run expanded after settle. The Settings page's
    /// General section owns the switch, and every open view (parked ones
    /// too) observes the app's `LayoutStore` global and re-folds when it flips.
    #[serde(rename = "foldFinishedTurns", default = "default_true")]
    pub fold_finished_turns: bool,
    /// Whether the terminal dock stands open in the centre column, under
    /// the composer (D42). Closed by default: the dock is asked for, never
    /// assumed.
    #[serde(rename = "terminalOpen", default)]
    pub terminal_open: bool,
    /// The settled dock height in window pixels, if the person ever set
    /// one. `None` is "never resized": the default height.
    #[serde(rename = "terminalHeight", default, skip_serializing_if = "Option::is_none")]
    pub terminal_height: Option<f32>,
    /// Where the terminal sits (docs/82 8.1): see [`TerminalPlace`]. While
    /// it is [`TerminalPlace::Pane`], [`Self::terminal_open`] means nothing
    /// and the pane's own state says whether the terminal shows.
    #[serde(rename = "terminalPlace", default, deserialize_with = "crate::store::lenient")]
    pub terminal_place: TerminalPlace,
    /// Whether the right pane stands open in the window's right column.
    /// Closed by default: the pane is asked for, never assumed, exactly
    /// as the dock is.
    #[serde(rename = "rightOpen", default)]
    pub right_open: bool,
    /// The settled right-pane width in window pixels, if the person ever
    /// set one. `None` is "never resized": the default width.
    #[serde(rename = "rightWidth", default, skip_serializing_if = "Option::is_none")]
    pub right_width: Option<f32>,
    /// Which of the three right-pane kinds was last shown, so reopening
    /// restores it. `None` is "never opened": [`RightKind::Files`]. A stored
    /// pre-merge `diff`/`git` reads back as [`RightKind::Changes`].
    #[serde(rename = "rightKind", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::store::lenient")]
    pub right_kind: Option<RightKind>,
    /// The kind the lower pane shows, stacked under the first (docs/82
    /// 8.1); `None` is no lower pane. The open session's value, mirrored
    /// here like [`Self::right_kind`] and restored per session.
    #[serde(rename = "rightLower", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::store::lenient")]
    pub right_lower: Option<RightKind>,
    /// The top pane's share of the pane's height while a lower pane shows;
    /// `None` is [`SPLIT_DEFAULT`]. Read it through [`Layout::split`].
    #[serde(rename = "rightSplit", default, skip_serializing_if = "Option::is_none")]
    pub right_split: Option<f32>,
    /// The top kind shown before the current one, while this window runs:
    /// where the pane goes back to when the terminal leaves it. Never
    /// stored.
    #[serde(skip)]
    pub right_previous: Option<RightKind>,
    /// The two inherit-owner-servers opt-ins, owned by the Settings →
    /// Providers page (see [`UseOwnMcp`]).
    #[serde(rename = "useOwnMcp", default)]
    pub use_own_mcp: UseOwnMcp,
    /// Which half of the window the person is in (B2a-H2): sessions or
    /// browse. `None` in an old file reads as sessions — today's Baaz —
    /// and `BAAZ_DETERMINISTIC=1` keeps that default, like every other
    /// preference, so captures never depend on the owner's last mode.
    #[serde(rename = "mode", default, deserialize_with = "crate::store::lenient")]
    pub mode: Mode,
    /// Whether the sidebar stands open: one state for both modes. Open by
    /// default, so a file without the key reads as a fresh window.
    #[serde(rename = "sidebarOpen", default = "default_true")]
    pub sidebar_open: bool,
    /// The Sessions right pane while the window is in Browse; `None` in
    /// Sessions, where `right_open` and `right_kind` are that pane. Kept in
    /// the file so a quit in Browse hands it back after the relaunch.
    #[serde(rename = "sessionsPane", default, skip_serializing_if = "Option::is_none")]
    pub sessions_pane: Option<SessionsPane>,
    /// The engine an address commit searches with (B2a-H6): what the
    /// Settings → Browser page owns. Missing reads as Google, like the
    /// old toolbar const.
    #[serde(rename = "searchEngine", default, deserialize_with = "crate::store::lenient")]
    pub search_engine: crate::browser_prefs::SearchEngine,
    /// Where downloads land (B2a-H6): `None` is `~/Downloads`. Owned by
    /// the Settings → Browser page's Change row; passed to the CEF
    /// backend at startup and on change.
    #[serde(rename = "downloadDir", default, skip_serializing_if = "Option::is_none")]
    pub download_dir: Option<PathBuf>,
    /// Whether the Files preview soft-wraps long lines (owner 2026-10-31:
    /// "some max width and some wrapping"). ON by default, as in an
    /// editor; the preview's Wrap button and ⌥Z flip it, and the choice
    /// holds for every file and across launches.
    #[serde(rename = "codeWrap", default = "default_true")]
    pub code_wrap: bool,
    /// Settings › Browser › Assistant (docs/27 §6, R3): the provider and
    /// model a browser chat starts with. Empty is the default.
    #[serde(rename = "assistant", default, skip_serializing_if = "crate::layout::is_default_assistant")]
    pub assistant: crate::browser_prefs::AssistantPrefs,
    /// Settings › Browser › Agents (docs/27 §6.2, R3).
    #[serde(rename = "agents", default, skip_serializing_if = "crate::layout::is_default_agents")]
    pub agents: crate::browser_prefs::agents::AgentsPrefs,
    /// Settings › Browser › Ad blocking (R4): on/off and the allowed sites.
    #[serde(rename = "adblock", default, skip_serializing_if = "crate::layout::is_default_adblock")]
    pub adblock: crate::browser_prefs::AdblockPrefs,
    /// Settings › Git & worktrees › Worktrees (docs/82 6.1): what a session's
    /// worktree branch starts with. `None` is
    /// [`crate::worktrees::DEFAULT_BRANCH_PREFIX`]; read it through
    /// [`Layout::worktree_branch_prefix`].
    #[serde(rename = "worktreeBranchPrefix", default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch_prefix: Option<String>,
    /// Settings › Git & worktrees: archive a session when the pull request of its
    /// branch reads merged (docs/82 7.3). Off by default.
    #[serde(rename = "archiveOnMerge", default)]
    pub archive_on_merge: bool,
    /// Settings › General › "Open files in" (docs/82 7.5): an editor id from
    /// [`crate::editors::KNOWN_EDITORS`], or `None` / `"default"` for macOS's
    /// own pick. Read it through [`crate::editors::chosen`], which falls back
    /// to the default when the editor is no longer installed.
    #[serde(rename = "openIn", default, skip_serializing_if = "Option::is_none")]
    pub open_in: Option<String>,
    /// The one-time hints (docs/82 8.7): the four regions named on the
    /// first composer, and the default-browser offer after the first use of
    /// Browse. Both `NotYet` in an old file, so a person who never met the
    /// first run never sees the regions strip.
    #[serde(rename = "onboarding", default)]
    pub onboarding: Onboarding,
}

/// Where one one-time hint stands (docs/82 8.7): never asked for, showing
/// until it is answered or dismissed, or done for good.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Once {
    /// Nothing has asked for it yet.
    #[default]
    NotYet,
    /// It shows wherever it belongs until it is answered or dismissed.
    Due,
    /// Answered or dismissed: it never shows again.
    Done,
}

/// The one-time hints (docs/82 8.7), kept with the other window
/// preferences in `layout.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Onboarding {
    /// The strip naming the window's four regions on the first composer:
    /// `Due` once the first run's folder step leads into it, `Done` when
    /// "Got it" or Escape dismisses it.
    #[serde(default, deserialize_with = "crate::store::lenient")]
    pub regions: Once,
    /// "Make Baaz your default browser?" above the Browse page: `Due` after
    /// the first page a person tab loaded in Browse, `Done` once answered
    /// or dismissed (or found moot: Baaz already is the default).
    #[serde(rename = "defaultBrowser", default, deserialize_with = "crate::store::lenient")]
    pub default_browser: Once,
}

impl Layout {
    /// The branch prefix a new worktree's branch takes: the stored one, else
    /// [`crate::worktrees::DEFAULT_BRANCH_PREFIX`].
    pub fn worktree_branch_prefix(&self) -> String {
        self.worktree_branch_prefix
            .as_deref()
            .and_then(normalize_branch_prefix)
            .unwrap_or_else(|| crate::worktrees::DEFAULT_BRANCH_PREFIX.to_owned())
    }
}

/// A branch prefix as typed, made safe for `git branch`: trimmed, spaces
/// to `-`, only ascii letters, digits and `/ - _ .`; `None` when nothing
/// usable is left (the default then stands).
pub fn normalize_branch_prefix(typed: &str) -> Option<String> {
    let cleaned: String = typed
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
        .collect();
    let cleaned = cleaned.trim_start_matches(['/', '-', '.']).replace("..", ".").replace("//", "/");
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Whether ad blocking is at its defaults (nothing stored).
fn is_default_adblock(prefs: &crate::browser_prefs::AdblockPrefs) -> bool {
    *prefs == crate::browser_prefs::AdblockPrefs::default()
}

/// Whether the Agents block is at its defaults (nothing stored).
fn is_default_agents(prefs: &crate::browser_prefs::agents::AgentsPrefs) -> bool {
    *prefs == crate::browser_prefs::agents::AgentsPrefs::default()
}

/// Whether the assistant pick is the default (nothing stored).
fn is_default_assistant(prefs: &crate::browser_prefs::AssistantPrefs) -> bool {
    *prefs == crate::browser_prefs::AssistantPrefs::default()
}

/// The folder downloads land in when nothing was ever chosen:
/// `~/Downloads` (`/tmp/Downloads` with no home). The CEF backend's own
/// default matches, so a fresh window agrees with it before anything is
/// stored.
pub fn default_download_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp")).join("Downloads")
}

/// The folder downloads land in: the chosen one, or the default. Pure,
/// so Settings and startup share it.
pub fn browser_download_dir(layout: &Layout) -> PathBuf {
    layout.download_dir.clone().unwrap_or_else(default_download_dir)
}

/// A path for the Settings row: `~/…` when under home, else the full
/// path. Pure, so tests pin it without touching the home dir.
pub fn tilde_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if path == home {
            return "~".to_owned();
        }
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// The default for the auto-title and auto-summary switches: ON. A missing
/// key (every `layout.json` written before this feature) reads as enabled.
fn default_true() -> bool {
    true
}

fn default_search_all() -> bool {
    true
}

impl Layout {
    /// What the person asked to be told about (Settings › Notifications,
    /// task 3.5): every event on, the badge on, no sound until changed.
    pub fn notify_prefs(&self) -> NotifyPrefs {
        self.notify.map(NotifySettings::prefs).unwrap_or_default()
    }

    /// Change one Notifications switch, starting from the defaults.
    pub fn set_notify(&mut self, edit: impl FnOnce(&mut NotifySettings)) {
        let mut settings = self.notify.unwrap_or_default();
        edit(&mut settings);
        self.notify = Some(settings);
    }

    /// Set the Sessions right pane aside for Browse, whose own pane stands
    /// closed on the default kind with no lower pane. A pane already aside
    /// (a relaunch into Browse) stays what Sessions gets back.
    pub fn enter_browse(&mut self) {
        let pane = SessionsPane { open: self.right_open, kind: self.right_kind, lower: self.right_lower, split: self.right_split };
        self.sessions_pane.get_or_insert(pane);
        self.right_open = false;
        self.right_kind = None;
        self.right_lower = None;
    }

    /// Hand Sessions its right pane back. Whatever Browse's pane did goes.
    /// A pane that comes back showing the terminal takes it (see
    /// [`Self::claim_terminal`]).
    pub fn leave_browse(&mut self) {
        if let Some(pane) = self.sessions_pane.take() {
            self.right_open = pane.open;
            self.right_kind = pane.kind;
            self.right_lower = pane.lower;
            self.right_split = pane.split;
            self.claim_terminal();
        } else {
            self.right_open = false;
            self.right_kind = None;
            self.right_lower = None;
        }
    }

    // ── the right pane's workspace (docs/82 8.1) ───────────────────────

    /// The top pane's share of the pane's height: the stored one, held to
    /// its range, or [`SPLIT_DEFAULT`].
    pub fn split(&self) -> f32 {
        self.right_split.map_or(SPLIT_DEFAULT, clamp_split)
    }

    /// The lower pane as drawn: in Sessions only (Browse keeps one body),
    /// a kind that may sit below and is not the one on top. Whether the
    /// pane itself stands open is the caller's to ask.
    pub fn shown_lower(&self) -> Option<RightKind> {
        if self.mode == Mode::Browse {
            return None;
        }
        self.right_lower.filter(|kind| kind.sits_below() && *kind != right_kind(self))
    }

    /// Whether the open pane shows the terminal, on top or below.
    pub fn terminal_in_pane(&self) -> bool {
        self.right_open && (right_kind(self) == RightKind::Terminal || self.shown_lower() == Some(RightKind::Terminal))
    }

    /// Whether the terminal shows anywhere: the open dock, or the pane's
    /// Terminal view.
    pub fn terminal_shown(&self) -> bool {
        match self.terminal_place {
            TerminalPlace::Bottom => self.terminal_open,
            TerminalPlace::Pane => self.terminal_in_pane(),
        }
    }

    /// Whether the dock under the composer stands open: the terminal sits
    /// at the bottom and is open there.
    pub fn dock_open(&self) -> bool {
        self.terminal_place == TerminalPlace::Bottom && self.terminal_open
    }

    /// One terminal, one place: a pane that shows the terminal takes it
    /// from the dock. Called after every change that can open the pane on
    /// it (a pick, a restore, Browse handing the pane back).
    pub fn claim_terminal(&mut self) {
        if self.terminal_in_pane() {
            self.terminal_place = TerminalPlace::Pane;
            self.terminal_open = false;
        }
    }

    /// Open the pane on `kind` (the top pane), remembering what showed
    /// before. A lower pane on the same kind closes (one kind shows once);
    /// the terminal moves into the pane when that is what it now shows.
    pub fn open_top(&mut self, kind: RightKind) {
        let was = right_kind(self);
        if was != kind {
            self.right_previous = Some(was);
        }
        self.right_kind = Some(kind);
        self.right_open = true;
        if self.right_lower == Some(kind) {
            self.right_lower = None;
        }
        self.claim_terminal();
    }

    /// Stack `kind` under the top pane, or close the lower pane (`None`).
    /// Refused — nothing changes — in Browse, for the Page, and for the
    /// kind already on top. The terminal moves into the pane when it is
    /// the lower kind and the pane stands open.
    pub fn set_lower(&mut self, kind: Option<RightKind>) -> Result<(), &'static str> {
        if let Some(kind) = kind {
            if self.mode == Mode::Browse {
                return Err("Browse keeps one body");
            }
            if !kind.sits_below() {
                return Err("the Page is top-only");
            }
            if kind == right_kind(self) {
                return Err("that kind is on top");
            }
        }
        self.right_lower = kind;
        self.claim_terminal();
        Ok(())
    }

    /// Put the terminal in the pane (the dock header's "Move terminal to
    /// the right pane", ⌃` there, the `terminal-place:pane` step): below an
    /// open Page — the reference's browser over terminal — and otherwise
    /// as the top view, opening the pane. The dock closes. Already in the
    /// pane and showing, nothing moves.
    pub fn seat_terminal_in_pane(&mut self) {
        self.terminal_place = TerminalPlace::Pane;
        self.terminal_open = false;
        if self.terminal_in_pane() {
            return;
        }
        if self.right_open && self.mode != Mode::Browse && right_kind(self) == RightKind::Browser {
            self.right_lower = Some(RightKind::Terminal);
        } else {
            self.open_top(RightKind::Terminal);
        }
    }

    /// The agent ran something in the terminal while it sits in the pane
    /// and does not show: open the pane on it when closed, else stack it
    /// below whatever the person is looking at (the top view stays). In
    /// Browse, where nothing stacks, it takes the top.
    pub fn reveal_terminal_in_pane(&mut self) {
        if self.terminal_in_pane() {
            return;
        }
        if self.right_open && self.mode != Mode::Browse {
            self.right_lower = Some(RightKind::Terminal);
            self.claim_terminal();
        } else {
            self.open_top(RightKind::Terminal);
        }
    }

    /// Dock the terminal at the bottom (the pane's "Dock terminal at the
    /// bottom", ⌃` with the terminal there, `terminal-place:bottom`): the
    /// dock opens, and the pane gives the terminal up — a top Terminal
    /// goes back to the view shown before it (Files when there was none),
    /// a lower one closes.
    pub fn dock_terminal_bottom(&mut self) {
        self.terminal_place = TerminalPlace::Bottom;
        self.terminal_open = true;
        if self.right_lower == Some(RightKind::Terminal) {
            self.right_lower = None;
        }
        if self.right_kind == Some(RightKind::Terminal) {
            let back = self.right_previous.filter(|kind| *kind != RightKind::Terminal).unwrap_or(RightKind::Files);
            self.right_kind = Some(back);
            if self.right_lower == Some(back) {
                self.right_lower = None;
            }
        }
    }

    /// ⌃`, wherever the terminal sits: the dock flips open or shut; in the
    /// pane a showing terminal goes (the pane closes when it is the top
    /// view, the lower pane when it is below) and a hidden one is seated
    /// ([`Self::seat_terminal_in_pane`]). Returns whether it now shows.
    pub fn toggle_terminal(&mut self) -> bool {
        match self.terminal_place {
            TerminalPlace::Bottom => self.terminal_open = !self.terminal_open,
            TerminalPlace::Pane if self.terminal_in_pane() => {
                if right_kind(self) == RightKind::Terminal {
                    self.right_open = false;
                } else {
                    self.right_lower = None;
                }
            }
            TerminalPlace::Pane => self.seat_terminal_in_pane(),
        }
        self.terminal_shown()
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_width: None,
            group_by: None,
            closed_groups: Vec::new(),
            expanded_groups: Vec::new(),
            search_all_projects: true,
            sort_by: SortBy::Recent,
            group_bar: false,
            theme: ThemePref::Dark,
            density: DensityPref::Comfortable,
            text_size: TextSizePref::Default,
            summary_line: false,
            notify: None,
            group_branch: false,
            auto_title: true,
            auto_summary: true,
            handoff_model_summary: true,
            fold_finished_turns: true,
            terminal_open: false,
            terminal_height: None,
            terminal_place: TerminalPlace::Bottom,
            right_open: false,
            right_width: None,
            right_kind: None,
            right_lower: None,
            right_split: None,
            right_previous: None,
            use_own_mcp: UseOwnMcp::default(),
            mode: Mode::Sessions,
            sidebar_open: true,
            sessions_pane: None,
            search_engine: crate::browser_prefs::SearchEngine::Google,
            download_dir: None,
            code_wrap: true,
            assistant: Default::default(),
            agents: Default::default(),
            adblock: Default::default(),
            worktree_branch_prefix: None,
            archive_on_merge: false,
            open_in: None,
            onboarding: Onboarding::default(),
        }
    }
}

/// The layout's one writer ([`crate::store::JsonStore`]); a deterministic
/// capture writes nothing (see [`write`]).
static STORE: crate::store::JsonStore<Layout> =
    crate::store::JsonStore::new("layout.json").with_encoding(encode);

fn encode(layout: &Layout) -> Option<Vec<u8>> {
    if crate::runtime::state_isolated() {
        return None;
    }
    serde_json::to_vec_pretty(layout).ok()
}

/// Read the store. Blocking; call it off the UI thread.
pub fn read() -> Layout {
    // A deterministic run ignores the store entirely.
    //
    // Captures used to boot from the person's own `layout.json`, so anything
    // they had dragged leaked into every screenshot: a sidebar nudged to
    // 249.3 px and a right pane dragged to its maximum both moved every
    // committed baseline, with no code change between the runs. A baseline
    // that depends on user state is not a regression detector, it is a
    // record of one machine's afternoon.
    if crate::runtime::state_isolated() {
        return Layout::default();
    }
    STORE.read()
}

/// Write the store, atomically.
///
/// A failed write is reported ([`crate::store::write_authored`]). A deterministic
/// capture writes nothing at all: like [`read`], the owner's file must
/// never gain a capture's widths or modes. Only the app's `Layout::save`
/// (and `Layout::update` through it) calls this: the one way a preference
/// reaches disk.
pub fn write(layout: &Layout) {
    let _ = STORE.write_now(layout);
}


/// The kind the right pane should show: the last one stored, or Files when
/// nothing was ever stored.
pub fn right_kind(layout: &Layout) -> RightKind {
    layout.right_kind.unwrap_or(RightKind::Files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retired Feedback Mode switch (removed in plan 4.11b): a stored
    /// `feedbackMode` key — or either older spelling — reads as nothing,
    /// never fails the file, and is gone after the next write. Unknown
    /// keys are ignored (no `deny_unknown_fields` on [`Layout`]).
    #[test]
    fn a_stored_feedback_mode_key_still_loads() {
        for key in ["feedbackMode", "feedback_mode", "feedback"] {
            let text = format!("{{\"{key}\":true,\"summaryLine\":true,\"mode\":\"browse\",\"terminalOpen\":true}}");
            let back: Layout =
                serde_json::from_str(&text).unwrap_or_else(|err| panic!("{key} must not fail the read: {err}"));
            assert!(back.summary_line, "the file's other keys still read beside {key}");
            assert_eq!(back.mode, Mode::Browse);
            assert!(back.terminal_open);
            let written = serde_json::to_string(&back).unwrap();
            assert!(!written.contains(key), "{key} is not written back: {written}");
        }
    }

    /// B2a-H2b: the mode reads tolerantly — `null`, a number, an object
    /// or an unknown slug all open Sessions, and none of them fails the
    /// whole file.
    #[test]
    fn a_non_string_or_unknown_mode_reads_as_sessions() {
        for raw in ["null", "7", "{\"slug\":\"browse\"}", "\"spaceship\"", "\"browse\""] {
            let text = format!("{{\"mode\":{raw}}}");
            let back: Layout = serde_json::from_str(&text)
                .unwrap_or_else(|_| panic!("mode {raw} must not fail the read"));
            assert_eq!(
                back.mode,
                if raw == "\"browse\"" { Mode::Browse } else { Mode::Sessions },
                "mode {raw} decodes"
            );
        }
        assert_eq!(Layout::default().mode, Mode::Sessions);
    }

    #[test]
    fn every_kind_parses_back_from_its_slug() {
        for kind in RightKind::ALL {
            assert_eq!(RightKind::parse(kind.slug()), Some(kind));
        }
        assert_eq!(RightKind::parse("nope"), None);
        assert_eq!(RightKind::parse(""), None);
        assert_eq!(RightKind::parse("DIFF"), None);
    }

    #[test]
    fn the_mode_defaults_to_sessions_and_round_trips() {
        assert_eq!(Layout::default().mode, Mode::Sessions);
        assert_eq!(Mode::Sessions.slug(), "sessions");
        assert_eq!(Mode::Browse.slug(), "browse");
        assert_eq!(Mode::parse("sessions"), Some(Mode::Sessions));
        assert_eq!(Mode::parse("browse"), Some(Mode::Browse));
        assert_eq!(Mode::parse("nope"), None);
        assert_eq!(Mode::parse(""), None);
        assert_eq!(Mode::parse("Sessions"), None);
        assert_eq!(Mode::Sessions.other(), Mode::Browse);
        assert_eq!(Mode::Browse.other(), Mode::Sessions);
        // Unknown stored modes read as sessions, never an error.
        let old: Layout = serde_json::from_str("{\"mode\":\"tabs\"}").unwrap();
        assert_eq!(old.mode, Mode::Sessions);
    }

    #[test]
    fn pre_merge_slugs_and_stores_load_as_changes() {
        // B8b: layouts persisted as `diff` or `git` load as `Changes`, and
        // every write lands as `changes` — the legacy kinds never reach a
        // file again. `parse` and `right_kind` map the in-memory aliases
        // onto Changes the same way.
        assert_eq!(RightKind::parse("diff"), Some(RightKind::Changes));
        assert_eq!(RightKind::parse("git"), Some(RightKind::Changes));
        assert_eq!(RightKind::parse("changes"), Some(RightKind::Changes));
        for slug in ["diff", "git", "changes"] {
            let layout: Layout = serde_json::from_str(&format!("{{\"rightKind\":{slug:?}}}")).unwrap();
            assert_eq!(layout.right_kind, Some(RightKind::Changes), "{slug} loads as Changes");
            assert_eq!(right_kind(&layout), RightKind::Changes, "{slug} reads as Changes");
            // And re-saving writes `changes`, migrating the legacy slug.
            let text = serde_json::to_string(&layout).unwrap();
            assert!(text.contains("\"rightKind\":\"changes\""), "{slug} re-saves as changes: {text}");
        }
        assert_eq!(serde_json::to_string(&RightKind::Changes).unwrap(), "\"changes\"");
        assert_eq!(serde_json::to_string(&RightKind::Files).unwrap(), "\"files\"");
        assert_eq!(serde_json::to_string(&RightKind::Browser).unwrap(), "\"browser\"");
        assert_eq!(RightKind::Changes.label(), "Changes");
        assert_eq!(RightKind::Changes.slug(), "changes");
        assert_eq!(serde_json::to_string(&RightKind::Tasks).unwrap(), "\"tasks\"");
        assert_eq!(RightKind::parse("tasks"), Some(RightKind::Tasks));
        assert_eq!(RightKind::Tasks.label(), "Tasks");
        // And the five screens are exactly Changes · Files · Page · Terminal · Tasks.
        assert_eq!(
            RightKind::ALL,
            [RightKind::Changes, RightKind::Files, RightKind::Browser, RightKind::Terminal, RightKind::Tasks]
        );
    }

    /// docs/82 8.1: the terminal's place reads Bottom from every file
    /// written before it, an unknown value reads Bottom without failing the
    /// file, and Pane round-trips.
    #[test]
    fn the_terminal_place_is_bottom_by_default_and_round_trips() {
        assert_eq!(Layout::default().terminal_place, TerminalPlace::Bottom);
        let old: Layout = serde_json::from_str("{\"terminalOpen\":true}").unwrap();
        assert_eq!(old.terminal_place, TerminalPlace::Bottom);
        assert!(old.dock_open() && old.terminal_shown());
        let odd: Layout = serde_json::from_str("{\"terminalPlace\":\"ceiling\",\"summaryLine\":true}").unwrap();
        assert_eq!(odd.terminal_place, TerminalPlace::Bottom, "an unknown place reads as the dock");
        assert!(odd.summary_line, "and fails nothing else");
        let pane = Layout { terminal_place: TerminalPlace::Pane, right_lower: Some(RightKind::Terminal), right_split: Some(0.3), ..Layout::default() };
        let text = serde_json::to_string(&pane).unwrap();
        assert!(text.contains("\"terminalPlace\":\"pane\"") && text.contains("\"rightLower\":\"terminal\"") && text.contains("\"rightSplit\":0.3"), "{text}");
        assert!(!text.contains("rightPrevious") && !text.contains("right_previous"), "the previous view is never stored: {text}");
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!((back.terminal_place, back.right_lower, back.right_split), (TerminalPlace::Pane, Some(RightKind::Terminal), Some(0.3)));
        // A file with no lower pane writes none.
        assert!(!serde_json::to_string(&Layout::default()).unwrap().contains("rightLower"));
    }

    /// The lower pane: one kind other than the top's, never the Page, never
    /// in Browse; the top taking the lower's kind closes it.
    #[test]
    fn the_lower_pane_keeps_to_its_rules() {
        let mut layout = Layout::default();
        layout.open_top(RightKind::Changes);
        assert_eq!(layout.set_lower(Some(RightKind::Browser)), Err("the Page is top-only"));
        assert_eq!(layout.set_lower(Some(RightKind::Changes)), Err("that kind is on top"));
        assert_eq!(layout.right_lower, None, "a refusal changes nothing");
        layout.set_lower(Some(RightKind::Files)).unwrap();
        assert_eq!(layout.shown_lower(), Some(RightKind::Files));
        // Another top keeps it; the top on the lower's kind closes it.
        layout.open_top(RightKind::Tasks);
        assert_eq!(layout.shown_lower(), Some(RightKind::Files));
        layout.open_top(RightKind::Files);
        assert_eq!(layout.right_lower, None, "the top took the lower's kind: the lower pane closed");
        // Browse draws one body and stacks nothing.
        layout.set_lower(Some(RightKind::Changes)).unwrap();
        layout.mode = Mode::Browse;
        assert_eq!(layout.shown_lower(), None);
        assert_eq!(layout.set_lower(Some(RightKind::Tasks)), Err("Browse keeps one body"));
        layout.mode = Mode::Sessions;
        // Browse sets the lower pane aside with the rest and hands it back.
        layout.right_split = Some(0.6);
        layout.enter_browse();
        assert_eq!(layout.right_lower, None);
        layout.leave_browse();
        assert_eq!((layout.right_lower, layout.right_split), (Some(RightKind::Changes), Some(0.6)));
        // Closing it.
        layout.set_lower(None).unwrap();
        assert_eq!(layout.shown_lower(), None);
    }

    /// The divider's share holds to 0.25..=0.75, a stored value included.
    #[test]
    fn the_divider_clamps() {
        assert_eq!(clamp_split(0.1), SPLIT_MIN);
        assert_eq!(clamp_split(0.9), SPLIT_MAX);
        assert_eq!(clamp_split(0.4), 0.4);
        assert_eq!(clamp_split(f32::NAN), SPLIT_DEFAULT);
        assert_eq!(Layout::default().split(), SPLIT_DEFAULT);
        let stored: Layout = serde_json::from_str("{\"rightSplit\":0.02}").unwrap();
        assert_eq!(stored.split(), SPLIT_MIN, "a hand-edited share is held too");
    }

    /// ⌃` follows the place: the dock flips at the bottom; in the pane a
    /// hidden terminal is seated and a showing one goes.
    #[test]
    fn toggling_the_terminal_follows_its_place() {
        let mut layout = Layout::default();
        assert!(layout.toggle_terminal(), "the dock opens");
        assert!(layout.dock_open());
        assert!(!layout.toggle_terminal(), "and shuts");
        // In the pane: closed pane → opens on Terminal.
        layout.terminal_place = TerminalPlace::Pane;
        assert!(layout.toggle_terminal());
        assert!(layout.right_open && right_kind(&layout) == RightKind::Terminal && !layout.dock_open());
        assert!(!layout.toggle_terminal(), "on top: the pane closes");
        assert!(!layout.right_open);
        // Over an open Page it goes below, and ⌃` closes only the lower pane.
        layout.open_top(RightKind::Browser);
        assert!(layout.toggle_terminal());
        assert_eq!((right_kind(&layout), layout.shown_lower()), (RightKind::Browser, Some(RightKind::Terminal)));
        assert!(!layout.toggle_terminal());
        assert!(layout.right_open && layout.right_lower.is_none(), "the Page stays");
    }

    /// Moving the terminal: into the pane closes the dock; back to the
    /// bottom opens it and gives the pane its previous view (Files with
    /// none), or closes the lower pane.
    #[test]
    fn moving_the_terminal_keeps_one_place() {
        let mut layout = Layout { terminal_open: true, ..Layout::default() };
        layout.open_top(RightKind::Changes);
        layout.seat_terminal_in_pane();
        assert_eq!(layout.terminal_place, TerminalPlace::Pane);
        assert!(!layout.terminal_open && layout.terminal_shown());
        assert_eq!(right_kind(&layout), RightKind::Terminal, "not over a Page: the top view");
        layout.dock_terminal_bottom();
        assert!(layout.dock_open());
        assert_eq!(right_kind(&layout), RightKind::Changes, "the view shown before the terminal");
        assert!(layout.right_open, "the pane stays open");
        // Below a Page, and back.
        layout.open_top(RightKind::Browser);
        layout.seat_terminal_in_pane();
        assert_eq!((right_kind(&layout), layout.shown_lower()), (RightKind::Browser, Some(RightKind::Terminal)));
        layout.seat_terminal_in_pane();
        assert_eq!(layout.shown_lower(), Some(RightKind::Terminal), "seating twice moves nothing");
        layout.dock_terminal_bottom();
        assert_eq!((right_kind(&layout), layout.right_lower), (RightKind::Browser, None));
        // A pane opened on the terminal (a restore, a pick) takes it.
        let mut layout = Layout { terminal_open: true, ..Layout::default() };
        layout.open_top(RightKind::Terminal);
        assert_eq!(layout.terminal_place, TerminalPlace::Pane);
        assert!(!layout.terminal_open, "the dock gave it up");
        // No previous view: Files.
        let mut layout = Layout { terminal_place: TerminalPlace::Pane, right_open: true, right_kind: Some(RightKind::Terminal), ..Layout::default() };
        layout.dock_terminal_bottom();
        assert_eq!(right_kind(&layout), RightKind::Files);
        // The agent's run reveals it below what the person looks at.
        let mut layout = Layout { terminal_place: TerminalPlace::Pane, ..Layout::default() };
        layout.open_top(RightKind::Changes);
        layout.reveal_terminal_in_pane();
        assert_eq!((right_kind(&layout), layout.shown_lower()), (RightKind::Changes, Some(RightKind::Terminal)));
    }

    /// The sidebar's one open state: open by default, and a closed one
    /// survives the round trip.
    #[test]
    fn sidebar_open_defaults_open_and_persists() {
        assert!(Layout::default().sidebar_open);
        let old: Layout = serde_json::from_str("{}").unwrap();
        assert!(old.sidebar_open, "a file without the key reads open");
        let closed = Layout { sidebar_open: false, ..Layout::default() };
        let text = serde_json::to_string(&closed).unwrap();
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert!(!back.sidebar_open);
    }

    /// B2a-H6: downloads land in `~/Downloads` until chosen, and the row
    /// shows `~/…` for home paths.
    #[test]
    fn download_dir_defaults_and_tildes() {
        let layout = Layout::default();
        assert_eq!(layout.download_dir, None);
        let dir = browser_download_dir(&layout);
        assert!(dir.ends_with("Downloads"), "the default is a Downloads folder: {}", dir.display());
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            assert_eq!(tilde_path(&home.join("Downloads")), "~/Downloads");
            assert_eq!(tilde_path(&home), "~");
        }
        assert_eq!(tilde_path(std::path::Path::new("/tmp/baaz-x")), "/tmp/baaz-x");
        let chosen =
            Layout { download_dir: Some(PathBuf::from("/tmp/baaz-picked")), ..Layout::default() };
        assert_eq!(browser_download_dir(&chosen), PathBuf::from("/tmp/baaz-picked"));
    }

    /// docs/82 8.7: a layout without the key never shows a first-run hint
    /// (a returning person never met the first run); a stored state reads
    /// back; an unknown value is read as never asked, losing nothing else.
    #[test]
    fn the_first_run_hints_are_kept_and_tolerant() {
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").expect("an old file");
        assert_eq!(old.onboarding.regions, Once::NotYet);
        assert_eq!(old.onboarding.default_browser, Once::NotYet);
        let stored = Layout { onboarding: Onboarding { regions: Once::Due, default_browser: Once::Done }, ..Layout::default() };
        let text = serde_json::to_string(&stored).expect("encodes");
        assert!(text.contains("\"onboarding\":{\"regions\":\"due\",\"defaultBrowser\":\"done\"}"), "{text}");
        let back: Layout = serde_json::from_str(&text).expect("decodes");
        assert_eq!(back.onboarding, stored.onboarding);
        let odd: Layout =
            serde_json::from_str("{\"sidebar_width\":300.0,\"onboarding\":{\"regions\":\"later\"}}").expect("tolerant");
        assert_eq!(odd.onboarding.regions, Once::NotYet);
        assert_eq!(odd.sidebar_width, Some(300.0));
    }
}
