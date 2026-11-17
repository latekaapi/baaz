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
//! The drag math lives here too, next to the persistence, so both the pointer
//! intents and the tests share the one clamped expression.

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

    /// The theme this choice paints with while the system's appearance is
    /// `system_dark`: the seam System follows the OS through. Pure, so the
    /// tests drive both appearances without a window.
    pub fn resolve(self, system_dark: bool) -> aui_tokens::ThemeKind {
        match self {
            ThemePref::System if system_dark => aui_tokens::ThemeKind::Dark,
            ThemePref::System => aui_tokens::ThemeKind::Light,
            ThemePref::Light => aui_tokens::ThemeKind::Light,
            ThemePref::Dark => aui_tokens::ThemeKind::Dark,
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

    /// The library's density for this choice.
    pub fn density(self) -> aui_tokens::Density {
        match self {
            DensityPref::Comfortable => aui_tokens::Density::Comfortable,
            DensityPref::Compact => aui_tokens::Density::Compact,
        }
    }

    /// The control's word for the choice.
    pub fn label(self) -> &'static str {
        self.density().label()
    }

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

    /// The library's text size for this choice.
    pub fn text_size(self) -> aui_tokens::TextSize {
        match self {
            TextSizePref::Default => aui_tokens::TextSize::Default,
            TextSizePref::Large => aui_tokens::TextSize::Large,
        }
    }

    /// The control's word for the choice.
    pub fn label(self) -> &'static str {
        self.text_size().label()
    }

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

/// Whether gpui's window appearance is a dark one (`darkAqua` or the
/// vibrant dark variant).
pub fn appearance_is_dark(appearance: gpui::WindowAppearance) -> bool {
    matches!(appearance, gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark)
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
            de::Error::unknown_variant(text.as_str(), &["browser", "changes", "files", "diff", "git"])
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
    /// Every kind the pane's tab row offers: Changes · Files · Page.
    pub const ALL: [RightKind; 3] =
        [RightKind::Changes, RightKind::Files, RightKind::Browser];

    /// The user-visible name: the tab row's labels, and T3's ⌘K labels.
    /// The legacy aliases label as the view they load into.
    pub fn label(self) -> &'static str {
        match self {
            // D6: the session's one page (docs/43 §2.7); `browser` stays the slug.
            RightKind::Browser => "Page",
            RightKind::Changes => "Changes",
            RightKind::Files => "Files",
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
            _ => None,
        }
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsPane {
    /// Whether the pane stood open.
    #[serde(default)]
    pub open: bool,
    /// Which kind it showed, if it was ever opened.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::store::lenient")]
    pub kind: Option<RightKind>,
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

/// Settings › Notifications, as stored (task 3.5). Every field defaults the
/// way [`crate::attention::NotifyPrefs::default`] does, so a partial
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
        let d = crate::attention::NotifyPrefs::default();
        Self { needs_you: d.needs_you, done: d.done, failed: d.failed, sound: d.sound, badge: d.badge }
    }
}

impl NotifySettings {
    fn prefs(self) -> crate::attention::NotifyPrefs {
        crate::attention::NotifyPrefs { needs_you: self.needs_you, done: self.done, failed: self.failed, sound: self.sound, badge: self.badge }
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
    /// too) observes [`LayoutStore`] and re-folds when it flips.
    #[serde(rename = "foldFinishedTurns", default = "default_true")]
    pub fold_finished_turns: bool,
    /// Whether ⌃⌥⌘F points at parts of Baaz to leave notes for Claude
    /// (F2). OFF by default: an old file reads as disabled, and the
    /// gesture toasts until the person opts in. The Settings page's
    /// General section owns the switch.
    #[serde(rename = "feedbackMode", default)]
    pub feedback_mode: bool,
    /// Whether the terminal dock stands open in the centre column, under
    /// the composer (D42). Closed by default: the dock is asked for, never
    /// assumed.
    #[serde(rename = "terminalOpen", default)]
    pub terminal_open: bool,
    /// The settled dock height in window pixels, if the person ever set
    /// one. `None` is "never resized": the default height.
    #[serde(rename = "terminalHeight", default, skip_serializing_if = "Option::is_none")]
    pub terminal_height: Option<f32>,
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
    pub search_engine: crate::browser::toolbar::SearchEngine,
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
    pub assistant: crate::browser::assistant::AssistantPrefs,
    /// Settings › Browser › Agents (docs/27 §6.2, R3).
    #[serde(rename = "agents", default, skip_serializing_if = "crate::layout::is_default_agents")]
    pub agents: crate::browser::agents::AgentsPrefs,
    /// Settings › Browser › Ad blocking (R4): on/off and the allowed sites.
    #[serde(rename = "adblock", default, skip_serializing_if = "crate::layout::is_default_adblock")]
    pub adblock: crate::browser::adblock::AdblockPrefs,
}

/// Whether ad blocking is at its defaults (nothing stored).
fn is_default_adblock(prefs: &crate::browser::adblock::AdblockPrefs) -> bool {
    *prefs == crate::browser::adblock::AdblockPrefs::default()
}

/// Whether the Agents block is at its defaults (nothing stored).
fn is_default_agents(prefs: &crate::browser::agents::AgentsPrefs) -> bool {
    *prefs == crate::browser::agents::AgentsPrefs::default()
}

/// Whether the assistant pick is the default (nothing stored).
fn is_default_assistant(prefs: &crate::browser::assistant::AssistantPrefs) -> bool {
    *prefs == crate::browser::assistant::AssistantPrefs::default()
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
    pub(crate) fn notify_prefs(&self) -> crate::attention::NotifyPrefs {
        self.notify.map(NotifySettings::prefs).unwrap_or_default()
    }

    /// Change one Notifications switch, starting from the defaults.
    pub(crate) fn set_notify(&mut self, edit: impl FnOnce(&mut NotifySettings)) {
        let mut settings = self.notify.unwrap_or_default();
        edit(&mut settings);
        self.notify = Some(settings);
    }

    /// Set the Sessions right pane aside for Browse, whose own pane stands
    /// closed on the default kind. A pane already aside (a relaunch into
    /// Browse) stays what Sessions gets back.
    pub fn enter_browse(&mut self) {
        let pane = SessionsPane { open: self.right_open, kind: self.right_kind };
        self.sessions_pane.get_or_insert(pane);
        self.right_open = false;
        self.right_kind = None;
    }

    /// Hand Sessions its right pane back. Whatever Browse's pane did goes.
    pub fn leave_browse(&mut self) {
        if let Some(pane) = self.sessions_pane.take() {
            self.right_open = pane.open;
            self.right_kind = pane.kind;
        } else {
            self.right_open = false;
            self.right_kind = None;
        }
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
            feedback_mode: false,
            terminal_open: false,
            terminal_height: None,
            right_open: false,
            right_width: None,
            right_kind: None,
            use_own_mcp: UseOwnMcp::default(),
            mode: Mode::Sessions,
            sidebar_open: true,
            sessions_pane: None,
            search_engine: crate::browser::toolbar::SearchEngine::Google,
            download_dir: None,
            code_wrap: true,
            assistant: Default::default(),
            agents: Default::default(),
            adblock: Default::default(),
        }
    }
}

/// The layout's one writer ([`crate::store::JsonStore`]); a deterministic
/// capture writes nothing (see [`write`]).
static STORE: crate::store::JsonStore<Layout> =
    crate::store::JsonStore::new("layout.json").with_encoding(encode);

fn encode(layout: &Layout) -> Option<Vec<u8>> {
    if crate::clock::deterministic() {
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
    if crate::clock::deterministic() {
        return Layout::default();
    }
    STORE.read()
}

/// Write the store, atomically.
///
/// A failed write is reported ([`crate::store::write_authored`]). A deterministic
/// capture writes nothing at all: like [`read`], the owner's file must
/// never gain a capture's widths or modes. Private: [`Layout::save`] (and
/// [`Layout::update`] through it) is the one way a preference reaches disk.
fn write(layout: &Layout) {
    let _ = STORE.write_now(layout);
}

/// The window's preferences as one gpui [`gpui::Global`] (task 4.1, audit D
/// Q1 row 7): the single live copy of `layout.json`. Every reader asks
/// [`Layout::get`]; every change goes through [`Layout::update`] (or
/// [`Layout::stage`]), which marks the global changed so each view that
/// called `cx.observe_global::<LayoutStore>` re-renders — no hand copy into
/// a view, no manual notify after a settings flip.
pub struct LayoutStore(Layout);

impl gpui::Global for LayoutStore {}

/// What [`Layout::get`] reads before the global is set: a standalone view in
/// a unit test that never booted a window.
static DEFAULT_LAYOUT: std::sync::LazyLock<Layout> = std::sync::LazyLock::new(Layout::default);

impl Layout {
    /// Read `layout.json` into the global, once: a second call keeps the live
    /// value. Boot calls it before anything reads a preference (`main`), and
    /// [`crate::app::Harness::new`] calls it for test windows.
    pub fn init(cx: &mut gpui::App) {
        if !cx.has_global::<LayoutStore>() {
            let layout = read();
            crate::browser::agents::set_current(&layout.agents);
            cx.set_global(LayoutStore(layout));
        }
    }

    /// The live preferences.
    pub fn get(cx: &gpui::App) -> &Layout {
        cx.try_global::<LayoutStore>().map_or(&DEFAULT_LAYOUT, |store| &store.0)
    }

    /// Change the preferences and write `layout.json`: the one write path.
    /// Observers of [`LayoutStore`] re-render on the change. The edit gets
    /// the layout only, never `cx`, so it cannot re-enter the global.
    pub fn update<R>(cx: &mut gpui::App, edit: impl FnOnce(&mut Layout) -> R) -> R {
        let result = Self::stage(cx, edit);
        Self::save(cx);
        result
    }

    /// Change the preferences without writing yet: the next
    /// [`Layout::update`] (or the quit's [`Layout::save`]) carries it. For a
    /// session switch restoring that session's pane, which must not pay for
    /// a file write, and for scripted steps and tests.
    pub fn stage<R>(cx: &mut gpui::App, edit: impl FnOnce(&mut Layout) -> R) -> R {
        Self::init(cx);
        // `global_mut` queues the observers' notification; unlike
        // `update_global` it leases nothing, so an observer that reads the
        // global (or updates it again) never finds it missing.
        let store = cx.global_mut::<LayoutStore>();
        edit(&mut store.0)
    }

    /// Write the live preferences, and put the Agents block in force for the
    /// page tools (whose jobs have no `cx`: [`crate::browser::agents::current`]
    /// is the one copy, and this is its only writer after boot).
    pub fn save(cx: &gpui::App) {
        let layout = Self::get(cx);
        crate::browser::agents::set_current(&layout.agents);
        write(layout);
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

/// The width the right pane should open at: the stored one, clamped into
/// the library range, or the default when nothing was ever stored.
pub fn right_width(layout: &Layout) -> f32 {
    match layout.right_width {
        Some(width) => aui::shell::clamp_right_width(width),
        None => aui::shell::RIGHT_WIDTH,
    }
}

/// The kind the right pane should show: the last one stored, or Files when
/// nothing was ever stored.
pub fn right_kind(layout: &Layout) -> RightKind {
    layout.right_kind.unwrap_or(RightKind::Files)
}

/// One drag move on the right pane's divider. The minus is the whole
/// difference from [`drag_width`]: the sidebar's divider sits on the
/// sidebar's RIGHT edge, so moving the pointer right widens it, while the
/// right pane's divider sits on the pane's LEFT edge, so moving the pointer
/// right NARROWS it.
pub fn right_drag_width(start_w: f32, grab_x: f32, x: f32) -> f32 {
    aui::shell::clamp_right_width(start_w - (x - grab_x))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic run must not read the person's store. This asserts the
    /// branch exists; the flag itself is process-wide, so the value under it
    /// is exercised by the probe rather than here.
    #[test]
    fn the_defaults_carry_no_right_pane_state() {
        let d = Layout::default();
        assert!(!d.right_open);
        assert_eq!(d.right_width, None);
        assert_eq!(d.right_kind, None);
        assert_eq!(d.sidebar_width, None);
        assert_eq!(sidebar_width(&d), aui::shell::SIDEBAR_WIDTH);
        assert_eq!(right_width(&d), aui::shell::RIGHT_WIDTH);
    }

    #[test]
    fn a_drag_moves_the_divider_by_the_pointer_delta() {
        assert_eq!(drag_width(252.0, 100.0, 120.0), 272.0);
        assert_eq!(drag_width(252.0, 100.0, 80.0), 232.0);
    }

    #[test]
    fn a_drag_never_leaves_the_library_range() {
        assert_eq!(drag_width(252.0, 0.0, -10_000.0), aui::shell::SIDEBAR_MIN_WIDTH);
        assert_eq!(drag_width(252.0, 0.0, 10_000.0), aui::shell::SIDEBAR_MAX_WIDTH);
        assert_eq!(drag_width(220.0, 200.0, 100.0), aui::shell::SIDEBAR_MIN_WIDTH);
        assert_eq!(drag_width(360.0, 200.0, 300.0), aui::shell::SIDEBAR_MAX_WIDTH);
    }

    #[test]
    fn an_empty_store_opens_at_the_default_width() {
        assert_eq!(sidebar_width(&Layout::default()), aui::shell::SIDEBAR_WIDTH);
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
    fn the_new_preferences_default_and_round_trip() {
        let layout = Layout::default();
        assert_eq!(layout.group_by, None);
        assert!(layout.closed_groups.is_empty());
        assert!(layout.expanded_groups.is_empty());
        assert!(layout.search_all_projects);
        assert!(!layout.group_bar);
        assert!(!layout.summary_line);
        assert!(!layout.group_branch);
        let stored = Layout {
            sidebar_width: Some(300.0),
            group_by: Some(GroupBy::Project),
            closed_groups: vec!["other".into()],
            expanded_groups: vec!["p-baaz".into()],
            search_all_projects: false,
            sort_by: SortBy::Recent,
            group_bar: true,
            theme: ThemePref::System,
            density: DensityPref::Compact,
            text_size: TextSizePref::Large,
            summary_line: true,
            notify: None,
            group_branch: true,
            auto_title: true,
            auto_summary: true,
            handoff_model_summary: true,
            fold_finished_turns: false,
            feedback_mode: true,
            terminal_open: true,
            terminal_height: Some(300.0),
            right_open: true,
            right_width: Some(420.0),
            right_kind: Some(RightKind::Changes),
            use_own_mcp: UseOwnMcp { codex: true, claude_code: false },
            mode: Mode::Browse,
            sidebar_open: false,
            sessions_pane: Some(SessionsPane { open: true, kind: Some(RightKind::Files) }),
            search_engine: crate::browser::toolbar::SearchEngine::Kagi,
            download_dir: Some(PathBuf::from("/tmp/baaz-downloads")),
            code_wrap: false,
            assistant: Default::default(),
            agents: Default::default(),
            adblock: Default::default(),
        };
        let text = serde_json::to_string(&stored).unwrap();
        assert!(text.contains("\"groupBy\":\"project\""));
        assert!(text.contains("\"closedGroups\":[\"other\"]"));
        assert!(text.contains("\"expandedGroups\":[\"p-baaz\"]"));
        assert!(text.contains("\"searchAllProjects\":false"));
        assert!(text.contains("\"groupBar\":true"));
        assert!(text.contains("\"summaryLine\":true"));
        assert!(text.contains("\"theme\":\"system\""));
        assert!(text.contains("\"groupBranch\":true"));
        assert!(text.contains("\"foldFinishedTurns\":false"));
        assert!(text.contains("\"feedbackMode\":true"));
        assert!(text.contains("\"terminalOpen\":true"));
        assert!(text.contains("\"terminalHeight\":300.0"));
        assert!(text.contains("\"rightOpen\":true"));
        assert!(text.contains("\"rightWidth\":420.0"));
        assert!(text.contains("\"rightKind\":\"changes\""));
        assert!(text.contains("\"useOwnMcp\":{\"codex\":true,\"claudeCode\":false}"));
        assert!(text.contains("\"mode\":\"browse\""));
        assert!(text.contains("\"sidebarOpen\":false"));
        assert!(text.contains("\"sessionsPane\":{\"open\":true,\"kind\":\"files\"}"));
        assert!(text.contains("\"searchEngine\":\"kagi\""));
        assert!(text.contains("\"downloadDir\":\"/tmp/baaz-downloads\""));
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!(back.group_by, Some(GroupBy::Project));
        assert_eq!(back.closed_groups, vec!["other".to_owned()]);
        assert_eq!(back.expanded_groups, vec!["p-baaz".to_owned()]);
        assert!(!back.search_all_projects);
        assert!(back.group_bar);
        assert!(back.summary_line);
        assert_eq!(back.theme, ThemePref::System);
        assert!(back.group_branch);
        assert!(!back.fold_finished_turns);
        assert!(back.feedback_mode);
        assert!(back.terminal_open);
        assert_eq!(back.terminal_height, Some(300.0));
        assert!(back.right_open);
        assert_eq!(back.right_width, Some(420.0));
        assert_eq!(back.right_kind, Some(RightKind::Changes));
        assert!(back.use_own_mcp.codex);
        assert!(!back.use_own_mcp.claude_code);
        assert_eq!(back.mode, Mode::Browse);
        assert!(!back.sidebar_open);
        assert_eq!(back.sessions_pane, Some(SessionsPane { open: true, kind: Some(RightKind::Files) }));
        assert_eq!(back.search_engine, crate::browser::toolbar::SearchEngine::Kagi);
        assert_eq!(back.download_dir, Some(PathBuf::from("/tmp/baaz-downloads")));
        // An old file with only a width still reads, taking the new defaults.
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").unwrap();
        assert_eq!(old.mode, Mode::Sessions);
        assert!(old.sidebar_open);
        assert_eq!(old.sessions_pane, None);
        assert_eq!(old.group_by, None);
        assert!(old.expanded_groups.is_empty());
        assert!(old.search_all_projects);
        assert!(!old.group_bar);
        assert!(!old.summary_line);
        // The retired `compactRows` key reads as nothing: a two-line choice
        // made under it lands on the new one-line default too.
        let retired: Layout = serde_json::from_str("{\"compactRows\":false}").unwrap();
        assert!(!retired.summary_line);
        // The retired `groupChevron` key (docs/83 §9, 16 Nov) reads as
        // nothing: the group label draws its chevron on hover or when
        // closed, with no switch.
        let retired: Layout = serde_json::from_str("{\"groupChevron\":true}").unwrap();
        assert!(!serde_json::to_string(&retired).unwrap().contains("groupChevron"));
        assert!(!old.group_branch);
        // Both auto switches default ON, old files included: new behaviour
        // without a migration.
        assert!(Layout::default().auto_title);
        assert!(Layout::default().auto_summary);
        assert!(Layout::default().handoff_model_summary);
        assert!(Layout::default().fold_finished_turns);
        // Feedback mode defaults OFF, old files included: the gesture
        // toasts until the person opts in.
        assert!(!Layout::default().feedback_mode);
        assert!(!old.feedback_mode);
        assert!(old.auto_title);
        assert!(old.auto_summary);
        assert!(old.handoff_model_summary);
        assert!(old.fold_finished_turns);
        // The Files preview wraps long lines, old files included; off
        // survives a round trip.
        assert!(Layout::default().code_wrap);
        assert!(old.code_wrap);
        let off = Layout { code_wrap: false, ..Layout::default() };
        let back: Layout = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!back.code_wrap);
        // The dock starts closed at its default height, old files included.
        assert!(!Layout::default().terminal_open);
        assert_eq!(Layout::default().terminal_height, None);
        assert!(!old.terminal_open);
        assert_eq!(old.terminal_height, None);
        // The right pane starts closed at its default width showing Files,
        // old files included.
        assert!(!Layout::default().right_open);
        assert_eq!(Layout::default().right_width, None);
        assert_eq!(Layout::default().right_kind, None);
        assert!(!old.right_open);
        assert_eq!(old.right_width, None);
        assert_eq!(old.right_kind, None);
        assert_eq!(right_kind(&Layout::default()), RightKind::Files);
        assert_eq!(right_kind(&old), RightKind::Files);
    }

    #[test]
    fn the_inherit_owner_servers_switch_defaults_off_and_round_trips() {
        // Z5: both switches default off (old files included — serde
        // default, no migration), and an opted-in file reads back.
        let default = Layout::default();
        assert!(!default.use_own_mcp.codex);
        assert!(!default.use_own_mcp.claude_code);
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").unwrap();
        assert!(!old.use_own_mcp.codex);
        assert!(!old.use_own_mcp.claude_code);
        let stored = Layout {
            use_own_mcp: UseOwnMcp { codex: true, claude_code: true },
            ..Layout::default()
        };
        let text = serde_json::to_string(&stored).unwrap();
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert!(back.use_own_mcp.codex);
        assert!(back.use_own_mcp.claude_code);
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
    fn browse_sets_the_sessions_pane_aside_and_hands_it_back() {
        let mut layout = Layout {
            right_open: true,
            right_width: Some(420.0),
            right_kind: Some(RightKind::Changes),
            ..Layout::default()
        };
        layout.enter_browse();
        assert!(!layout.right_open, "Browse stands with its pane closed");
        assert_eq!(layout.right_kind, None, "on the default kind");
        assert_eq!(layout.right_width, Some(420.0), "one width for both modes");
        // Browse's own pane comes and goes; Sessions never sees it.
        layout.right_open = true;
        layout.right_kind = Some(RightKind::Files);
        let text = serde_json::to_string(&layout).unwrap();
        layout.leave_browse();
        assert!(layout.right_open);
        assert_eq!(layout.right_kind, Some(RightKind::Changes));
        assert_eq!(layout.sessions_pane, None, "Sessions owns the plain fields again");
        // A quit in Browse: the relaunch keeps what was set aside.
        let mut relaunched: Layout = serde_json::from_str(&text).unwrap();
        relaunched.enter_browse();
        assert!(!relaunched.right_open);
        relaunched.leave_browse();
        assert!(relaunched.right_open);
        assert_eq!(relaunched.right_kind, Some(RightKind::Changes));
    }

    /// Files written while each mode kept its own layout still load, and
    /// the next write leaves those keys behind. Their per-mode values are
    /// not read: the sidebar reads open, the plain fields stand.
    #[test]
    fn a_file_with_per_mode_keys_still_loads() {
        let text = r#"{
            "sidebar_width": 300.0,
            "rightOpen": true,
            "mode": "browse",
            "modeMemory": {
                "sessions": {
                    "sidebarOpen": false,
                    "rightOpen": true,
                    "rightWidth": 420.0,
                    "rightKind": "changes",
                    "scroll": {"item": 3, "offset": 4.0},
                    "selectedSession": "s-1"
                },
                "browse": {"sidebarOpen": false, "selectedTab": 7}
            },
            "sessions": {"rightOpen": true},
            "browse": {"selectedTab": 7}
        }"#;
        let layout: Layout = serde_json::from_str(text).unwrap();
        assert_eq!(layout.sidebar_width, Some(300.0), "the rest of the file survives");
        assert_eq!(layout.mode, Mode::Browse);
        assert!(layout.right_open);
        assert!(layout.sidebar_open);
        let written = serde_json::to_string(&layout).unwrap();
        for key in ["modeMemory", "selectedSession", "selectedTab"] {
            assert!(!written.contains(key), "{key} stays out of the next write");
        }
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
        // And the three live tabs are exactly Changes · Files · Browser.
        assert_eq!(RightKind::ALL, [RightKind::Changes, RightKind::Files, RightKind::Browser]);
    }

    #[test]
    fn a_right_drag_moves_the_divider_against_the_pointer() {
        // The divider sits on the pane's LEFT edge: moving the pointer
        // right narrows the pane, moving it left widens it — the opposite
        // of the sidebar's divider.
        assert_eq!(right_drag_width(400.0, 100.0, 120.0), 380.0);
        assert_eq!(right_drag_width(400.0, 100.0, 80.0), 420.0);
    }

    #[test]
    fn a_right_drag_never_leaves_the_library_range() {
        assert_eq!(right_drag_width(400.0, 0.0, -10_000.0), aui::shell::RIGHT_MAX_WIDTH);
        assert_eq!(right_drag_width(400.0, 0.0, 10_000.0), aui::shell::RIGHT_MIN_WIDTH);
        assert_eq!(right_drag_width(700.0, 200.0, 100.0), aui::shell::RIGHT_MAX_WIDTH);
        assert_eq!(right_drag_width(300.0, 200.0, 300.0), aui::shell::RIGHT_MIN_WIDTH);
    }

    #[test]
    fn an_empty_store_opens_the_right_pane_at_the_default_width() {
        assert_eq!(right_width(&Layout::default()), aui::shell::RIGHT_WIDTH);
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

    #[test]
    fn sort_by_defaults_to_recent_and_round_trips() {
        assert_eq!(SortBy::default(), SortBy::Recent);
        assert_eq!(Layout::default().sort_by, SortBy::Recent);
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").unwrap();
        assert_eq!(old.sort_by, SortBy::Recent);
        for sort in [SortBy::Created, SortBy::Name] {
            let layout = Layout { sort_by: sort, ..Layout::default() };
            let text = serde_json::to_string(&layout).unwrap();
            assert!(text.contains("\"sortBy\""), "{text}");
            let back: Layout = serde_json::from_str(&text).unwrap();
            assert_eq!(back.sort_by, sort);
        }
    }

    /// Settings › Appearance › Density and Text size (task 2.10): the
    /// design's values when nothing is stored, an unknown value never fails
    /// the file, each choice round-trips and maps onto the library's.
    #[test]
    fn density_and_text_size_persist() {
        assert_eq!(Layout::default().density, DensityPref::Comfortable);
        assert_eq!(Layout::default().text_size, TextSizePref::Default);
        let odd: Layout = serde_json::from_str("{\"density\":\"cosy\",\"textSize\":\"huge\"}").unwrap();
        assert_eq!((odd.density, odd.text_size), (DensityPref::Comfortable, TextSizePref::Default));
        for (density, text_size) in [(DensityPref::Compact, TextSizePref::Large), (DensityPref::Comfortable, TextSizePref::Default)] {
            let stored = Layout { density, text_size, ..Layout::default() };
            let text = serde_json::to_string(&stored).unwrap();
            let back: Layout = serde_json::from_str(&text).unwrap();
            assert_eq!((back.density, back.text_size), (density, text_size));
            assert_eq!(DensityPref::parse(density.label()), Some(density));
            assert_eq!(TextSizePref::parse(text_size.label()), Some(text_size));
        }
        assert!(serde_json::to_string(&Layout { density: DensityPref::Compact, ..Layout::default() }).unwrap().contains("\"density\":\"compact\""));
        assert_eq!(DensityPref::Compact.density(), aui_tokens::Density::Compact);
        assert_eq!(TextSizePref::Large.text_size(), aui_tokens::TextSize::Large);
    }

    /// Settings › Appearance › Theme (task 2.9): dark when nothing is
    /// stored (old files and unknown values included), each choice round
    /// trips, and System follows the OS appearance through `resolve`.
    #[test]
    fn the_theme_choice_persists_and_system_follows_the_os() {
        use aui_tokens::ThemeKind;
        assert_eq!(Layout::default().theme, ThemePref::Dark);
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").unwrap();
        assert_eq!(old.theme, ThemePref::Dark);
        let odd: Layout = serde_json::from_str("{\"theme\":\"sepia\"}").unwrap();
        assert_eq!(odd.theme, ThemePref::Dark, "an unknown theme never fails the file");
        for pref in ThemePref::ALL {
            let stored = Layout { theme: pref, ..Layout::default() };
            let back: Layout = serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
            assert_eq!(back.theme, pref);
            assert_eq!(ThemePref::parse(pref.label()), Some(pref));
        }
        assert_eq!(ThemePref::parse("sepia"), None);
        assert_eq!(ThemePref::System.resolve(true), ThemeKind::Dark);
        assert_eq!(ThemePref::System.resolve(false), ThemeKind::Light);
        for system_dark in [true, false] {
            assert_eq!(ThemePref::Light.resolve(system_dark), ThemeKind::Light);
            assert_eq!(ThemePref::Dark.resolve(system_dark), ThemeKind::Dark);
        }
        assert!(appearance_is_dark(gpui::WindowAppearance::Dark));
        assert!(appearance_is_dark(gpui::WindowAppearance::VibrantDark));
        assert!(!appearance_is_dark(gpui::WindowAppearance::Light));
        assert!(!appearance_is_dark(gpui::WindowAppearance::VibrantLight));
    }
}
