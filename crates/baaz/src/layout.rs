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

/// Which right-pane kind was last shown, so reopening restores it. Task
/// T3's ⌘K rows reuse [`RightKind::label`]; task T4's `right:<kind>` step
/// verb parses [`RightKind::slug`].
///
/// B8 merged the old Diff review and Git changes into one [`RightKind::Changes`]
/// view (this session's edits on top, the rest of the working tree below).
/// B8b serialises every load AND write as `changes`: the `Diff` and `Git`
/// variants stay so the call sites outside the B8b file set still compile,
/// but they never reach a file — [`RightKind::parse`] maps the stored
/// `diff`/`git` slugs onto `Changes`, and serialising any of
/// `Changes`/`Diff`/`Git` writes `changes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightKind {
    /// An embedded browser.
    Browser,
    /// This session's edits over the rest of the working tree.
    Changes,
    /// The workspace file tree.
    Files,
    /// Legacy alias of [`RightKind::Changes`] (the old Diff review).
    /// Retained so the call sites outside the B8b file set keep compiling;
    /// serde never produces it (see the impls above).
    #[allow(dead_code)]
    Diff,
    /// Legacy alias of [`RightKind::Changes`] (the old Git changes).
    /// Retained like [`RightKind::Diff`].
    #[allow(dead_code)]
    Git,
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
    /// Every kind the header tabs offer: Changes · Files · Browser.
    pub const ALL: [RightKind; 3] =
        [RightKind::Changes, RightKind::Files, RightKind::Browser];

    /// The user-visible name: the header tabs' labels, and T3's ⌘K labels.
    /// The legacy aliases label as the view they load into.
    pub fn label(self) -> &'static str {
        match self {
            RightKind::Browser => "Browser",
            RightKind::Changes | RightKind::Diff | RightKind::Git => "Changes",
            RightKind::Files => "Files",
        }
    }

    /// The machine name: the header tabs' ids, and T4's `right:<kind>` verb
    /// parses these back. The legacy aliases slug as the kind they load
    /// into, so a stored `diff`/`git` round-trips through `changes`.
    pub fn slug(self) -> &'static str {
        match self {
            RightKind::Browser => "browser",
            RightKind::Changes | RightKind::Diff | RightKind::Git => "changes",
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
            "browser" => Some(RightKind::Browser),
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

    /// The user-visible name: the sidebar switch's segment and the rail's
    /// toggle label name the mode they show, not the one they leave.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Sessions => "Sessions",
            Mode::Browse => "Browse",
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

/// A sidebar scroll position in persistable form: the list item and the
/// pixels into it, the same pair [`gpui::ListOffset`] carries. Pure
/// data, so each mode's offset survives a restart in `layout.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScrollPos {
    /// The top item's index.
    #[serde(default)]
    pub item: usize,
    /// The pixels into the top item.
    #[serde(default)]
    pub offset: f32,
}

/// What one mode remembers while the person is in the other (B2a-H2):
/// the right pane's open state, kind and width, the sidebar's scroll
/// offset, and which session or tab was selected. Saved on the way out,
/// restored on the way back in — see [`ModeSlots::switch`].
///
/// B2a-H6: the sidebar's open state rides along too, so hiding the
/// sidebar in Browse never touches Sessions' own sidebar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModeState {
    /// Whether the sidebar stood open in this mode. Open by default, so
    /// a file written before this feature reads as today's behaviour.
    #[serde(rename = "sidebarOpen", default = "default_true")]
    pub sidebar_open: bool,
    /// Whether the right pane stood open in this mode.
    #[serde(rename = "rightOpen", default)]
    pub right_open: bool,
    /// The settled right-pane width in window pixels, if ever set here.
    #[serde(rename = "rightWidth", default, skip_serializing_if = "Option::is_none")]
    pub right_width: Option<f32>,
    /// Which right-pane kind was last shown here, if ever opened here.
    #[serde(rename = "rightKind", default, skip_serializing_if = "Option::is_none")]
    pub right_kind: Option<RightKind>,
    /// The sidebar's scroll offset while in this mode, if ever scrolled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll: Option<ScrollPos>,
    /// The selected session while in Sessions mode, if any was open.
    #[serde(rename = "selectedSession", default, skip_serializing_if = "Option::is_none")]
    pub selected_session: Option<String>,
    /// The selected person-tab id while in Browse mode, if any.
    #[serde(rename = "selectedTab", default, skip_serializing_if = "Option::is_none")]
    pub selected_tab: Option<u64>,
}

impl Default for ModeState {
    /// Open sidebar, everything else shut: an untouched mode reads — and
    /// stays out of `layout.json` — exactly like before this feature.
    fn default() -> Self {
        Self {
            sidebar_open: true,
            right_open: false,
            right_width: None,
            right_kind: None,
            scroll: None,
            selected_session: None,
            selected_tab: None,
        }
    }
}

impl ModeState {
    /// Whether nothing was ever remembered here: what keeps an untouched
    /// mode out of `layout.json`, so old files read unchanged.
    pub fn is_default(&self) -> bool {
        *self == ModeState::default()
    }
}

/// Both modes' remembered states, as one `layout.json` object.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModeSlots {
    /// What Sessions mode remembers.
    #[serde(default, skip_serializing_if = "ModeState::is_default")]
    pub sessions: ModeState,
    /// What Browse mode remembers.
    #[serde(default, skip_serializing_if = "ModeState::is_default")]
    pub browse: ModeState,
}

impl ModeSlots {
    /// The remembered state for `mode`.
    pub fn get(&self, mode: Mode) -> &ModeState {
        match mode {
            Mode::Sessions => &self.sessions,
            Mode::Browse => &self.browse,
        }
    }

    /// Leave `from` carrying `leaving` and answer with what `to`
    /// remembered: the one pure step behind every mode switch, so the
    /// leaving mode's state is saved before the entering mode's is
    /// restored, never the other way round.
    pub fn switch(&mut self, from: Mode, leaving: ModeState, to: Mode) -> ModeState {
        match from {
            Mode::Sessions => self.sessions = leaving,
            Mode::Browse => self.browse = leaving,
        }
        self.get(to).clone()
    }

    /// Whether neither mode remembered anything: what keeps the whole
    /// object out of `layout.json` until the first real switch.
    pub fn is_default_all(&self) -> bool {
        self.sessions.is_default() && self.browse.is_default()
    }
}

/// Where Browse shows the person's tabs (B2a-H6): down the sidebar, as
/// the window always did — or in a strip row above the page, with the
/// sidebar keeping only New tab, From sessions and Agents. Owned by the
/// Settings → Browser page; changing it is instant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserTabs {
    /// The tab list down the sidebar.
    #[default]
    Vertical,
    /// A tab strip row above the page.
    Horizontal,
}

impl BrowserTabs {
    /// Every layout the Settings segmented control offers, in order.
    pub const ALL: [BrowserTabs; 2] = [BrowserTabs::Vertical, BrowserTabs::Horizontal];

    /// The machine name: what `layout.json` stores and `settings-set:`
    /// parses.
    pub fn slug(self) -> &'static str {
        match self {
            BrowserTabs::Vertical => "vertical",
            BrowserTabs::Horizontal => "horizontal",
        }
    }

    /// The inverse of [`BrowserTabs::slug`]: `None` for an unknown string,
    /// so a hand-edited file reads as today's behaviour, never an error.
    pub fn parse(s: &str) -> Option<BrowserTabs> {
        match s.trim().to_ascii_lowercase().as_str() {
            "vertical" => Some(BrowserTabs::Vertical),
            "horizontal" => Some(BrowserTabs::Horizontal),
            _ => None,
        }
    }

    /// The user-visible name: the Settings segmented control's labels.
    pub fn label(self) -> &'static str {
        match self {
            BrowserTabs::Vertical => "Vertical",
            BrowserTabs::Horizontal => "Horizontal",
        }
    }
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
    /// Whether project group rows draw the collapse chevron in the leading
    /// box. Off by default: the row is a plain label.
    /// The Settings dialog's Sidebar section (⌘,) owns the switch; `--steps
    /// group-chevron` flips it for captures meanwhile.
    #[serde(rename = "groupChevron", default)]
    pub group_chevron: bool,
    /// Retained for old `layout.json` files and the `group-bar` step verb:
    /// no longer gates anything. The current project's 2 px accent mark
    /// draws from `current(true)` alone (agentic-ui v0.3.15), so this flag
    /// is accepted and ignored — Settings no longer offers it.
    #[serde(rename = "groupBar", default)]
    pub group_bar: bool,
    /// Compact sidebar rows: one line per session (title only) instead of
    /// the comfortable two (title plus the context/byline line). Off by
    /// default: rows draw comfortable. Owned by the Settings page's Sidebar
    /// section — see [`Self::group_chevron`].
    #[serde(rename = "compactRows", default)]
    pub compact_rows: bool,
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
    /// General section owns the switch, and every open view applies it
    /// live through [`crate::settings::apply_setting`].
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
    #[serde(rename = "rightKind", default, skip_serializing_if = "Option::is_none")]
    pub right_kind: Option<RightKind>,
    /// The two inherit-owner-servers opt-ins, owned by the Settings →
    /// Providers page (see [`UseOwnMcp`]).
    #[serde(rename = "useOwnMcp", default)]
    pub use_own_mcp: UseOwnMcp,
    /// Which half of the window the person is in (B2a-H2): sessions or
    /// browse. `None` in an old file reads as sessions — today's Baaz —
    /// and `BAAZ_DETERMINISTIC=1` keeps that default, like every other
    /// preference, so captures never depend on the owner's last mode.
    #[serde(rename = "mode", default)]
    pub mode: Mode,
    /// What each mode remembers while the person is in the other: the
    /// right pane's open state, kind and width, the sidebar scroll and
    /// the selection. Untouched modes stay out of the file, so old files
    /// read unchanged.
    #[serde(rename = "modeMemory", default, skip_serializing_if = "ModeSlots::is_default_all")]
    pub mode_memory: ModeSlots,
    /// Where Browse shows the person's tabs (B2a-H6): [`BrowserTabs`].
    /// Missing in every file written before this feature, which reads as
    /// vertical — today's Baaz.
    #[serde(rename = "browserTabs", default)]
    pub browser_tabs: BrowserTabs,
    /// The engine an address commit searches with (B2a-H6): what the
    /// Settings → Browser page owns. Missing reads as Google, like the
    /// old toolbar const.
    #[serde(rename = "searchEngine", default)]
    pub search_engine: crate::browser::toolbar::SearchEngine,
    /// Where downloads land (B2a-H6): `None` is `~/Downloads`. Owned by
    /// the Settings → Browser page's Change row; passed to the CEF
    /// backend at startup and on change.
    #[serde(rename = "downloadDir", default, skip_serializing_if = "Option::is_none")]
    pub download_dir: Option<PathBuf>,
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
            compact_rows: false,
            group_branch: false,
            auto_title: true,
            auto_summary: true,
            handoff_model_summary: true,
            fold_finished_turns: true,
            terminal_open: false,
            terminal_height: None,
            right_open: false,
            right_width: None,
            right_kind: None,
            use_own_mcp: UseOwnMcp::default(),
            mode: Mode::Sessions,
            mode_memory: ModeSlots::default(),
            browser_tabs: BrowserTabs::Vertical,
            search_engine: crate::browser::toolbar::SearchEngine::Google,
            download_dir: None,
        }
    }
}

/// `~/Library/Application Support/baaz/layout.json`.
pub fn path() -> PathBuf {
    crate::store::support_dir().join("layout.json")
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
    crate::store::read_json(&path())
}

/// Write the store, atomically.
///
/// Best-effort: a store that cannot be written loses a width, which is a
/// nuisance, and never a session, which would be a loss. A deterministic
/// capture writes nothing at all: like [`read`], the owner's file must
/// never gain a capture's widths, modes or per-mode memory.
pub fn write(layout: &Layout) {
    if crate::clock::deterministic() {
        return;
    }
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

/// The width the right pane should open at: the stored one, clamped into
/// the library range, or the default when nothing was ever stored.
pub fn right_width(layout: &Layout) -> f32 {
    match layout.right_width {
        Some(width) => aui::shell::clamp_right_width(width),
        None => aui::shell::RIGHT_WIDTH,
    }
}

/// The kind the right pane should show: the last one stored, or Files when
/// nothing was ever stored. A stored pre-merge `diff`/`git` (written before
/// B8, or constructed by a legacy call site) normalises to `Changes` here,
/// so every reader below sees only the three live kinds.
pub fn right_kind(layout: &Layout) -> RightKind {
    match layout.right_kind.unwrap_or(RightKind::Files) {
        RightKind::Diff | RightKind::Git => RightKind::Changes,
        kind => kind,
    }
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
        assert_eq!(drag_width(180.0, 200.0, 100.0), aui::shell::SIDEBAR_MIN_WIDTH);
        assert_eq!(drag_width(420.0, 200.0, 300.0), aui::shell::SIDEBAR_MAX_WIDTH);
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
        assert!(!layout.group_chevron);
        assert!(!layout.group_bar);
        assert!(!layout.compact_rows);
        assert!(!layout.group_branch);
        let stored = Layout {
            sidebar_width: Some(300.0),
            group_by: Some(GroupBy::Project),
            closed_groups: vec!["other".into()],
            expanded_groups: vec!["p-baaz".into()],
            search_all_projects: false,
            group_chevron: true,
            group_bar: true,
            compact_rows: true,
            group_branch: true,
            auto_title: true,
            auto_summary: true,
            handoff_model_summary: true,
            fold_finished_turns: false,
            terminal_open: true,
            terminal_height: Some(300.0),
            right_open: true,
            right_width: Some(420.0),
            right_kind: Some(RightKind::Changes),
            use_own_mcp: UseOwnMcp { codex: true, claude_code: false },
            mode: Mode::Browse,
            mode_memory: ModeSlots {
                sessions: ModeState {
                    sidebar_open: true,
                    right_open: true,
                    right_width: Some(420.0),
                    right_kind: Some(RightKind::Changes),
                    scroll: None,
                    selected_session: Some("s-1".to_owned()),
                    selected_tab: None,
                },
                browse: ModeState::default(),
            },
            browser_tabs: BrowserTabs::Horizontal,
            search_engine: crate::browser::toolbar::SearchEngine::Kagi,
            download_dir: Some(PathBuf::from("/tmp/baaz-downloads")),
        };
        let text = serde_json::to_string(&stored).unwrap();
        assert!(text.contains("\"groupBy\":\"project\""));
        assert!(text.contains("\"closedGroups\":[\"other\"]"));
        assert!(text.contains("\"expandedGroups\":[\"p-baaz\"]"));
        assert!(text.contains("\"searchAllProjects\":false"));
        assert!(text.contains("\"groupChevron\":true"));
        assert!(text.contains("\"groupBar\":true"));
        assert!(text.contains("\"compactRows\":true"));
        assert!(text.contains("\"groupBranch\":true"));
        assert!(text.contains("\"foldFinishedTurns\":false"));
        assert!(text.contains("\"terminalOpen\":true"));
        assert!(text.contains("\"terminalHeight\":300.0"));
        assert!(text.contains("\"rightOpen\":true"));
        assert!(text.contains("\"rightWidth\":420.0"));
        assert!(text.contains("\"rightKind\":\"changes\""));
        assert!(text.contains("\"useOwnMcp\":{\"codex\":true,\"claudeCode\":false}"));
        assert!(text.contains("\"mode\":\"browse\""));
        assert!(text.contains("\"modeMemory\":{\"sessions\":{\"sidebarOpen\":true,\"rightOpen\":true"));
        assert!(text.contains("\"browserTabs\":\"horizontal\""));
        assert!(text.contains("\"searchEngine\":\"kagi\""));
        assert!(text.contains("\"downloadDir\":\"/tmp/baaz-downloads\""));
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!(back.group_by, Some(GroupBy::Project));
        assert_eq!(back.closed_groups, vec!["other".to_owned()]);
        assert_eq!(back.expanded_groups, vec!["p-baaz".to_owned()]);
        assert!(!back.search_all_projects);
        assert!(back.group_chevron);
        assert!(back.group_bar);
        assert!(back.compact_rows);
        assert!(back.group_branch);
        assert!(!back.fold_finished_turns);
        assert!(back.terminal_open);
        assert_eq!(back.terminal_height, Some(300.0));
        assert!(back.right_open);
        assert_eq!(back.right_width, Some(420.0));
        assert_eq!(back.right_kind, Some(RightKind::Changes));
        assert!(back.use_own_mcp.codex);
        assert!(!back.use_own_mcp.claude_code);
        assert_eq!(back.mode, Mode::Browse);
        assert!(back.mode_memory.sessions.right_open);
        assert_eq!(back.mode_memory.sessions.right_width, Some(420.0));
        assert_eq!(back.mode_memory.sessions.right_kind, Some(RightKind::Changes));
        assert_eq!(back.mode_memory.sessions.selected_session.as_deref(), Some("s-1"));
        assert!(back.mode_memory.sessions.sidebar_open);
        assert!(back.mode_memory.browse.is_default());
        assert_eq!(back.browser_tabs, BrowserTabs::Horizontal);
        assert_eq!(back.search_engine, crate::browser::toolbar::SearchEngine::Kagi);
        assert_eq!(back.download_dir, Some(PathBuf::from("/tmp/baaz-downloads")));
        // An old file with only a width still reads, taking the new defaults.
        let old: Layout = serde_json::from_str("{\"sidebar_width\":300.0}").unwrap();
        assert_eq!(old.mode, Mode::Sessions);
        assert!(old.mode_memory.is_default_all());
        assert_eq!(old.group_by, None);
        assert!(old.expanded_groups.is_empty());
        assert!(old.search_all_projects);
        assert!(!old.group_chevron);
        assert!(!old.group_bar);
        assert!(!old.compact_rows);
        assert!(!old.group_branch);
        // Both auto switches default ON, old files included: new behaviour
        // without a migration.
        assert!(Layout::default().auto_title);
        assert!(Layout::default().auto_summary);
        assert!(Layout::default().handoff_model_summary);
        assert!(Layout::default().fold_finished_turns);
        assert!(old.auto_title);
        assert!(old.auto_summary);
        assert!(old.handoff_model_summary);
        assert!(old.fold_finished_turns);
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
        assert_eq!(Mode::Sessions.label(), "Sessions");
        assert_eq!(Mode::Browse.label(), "Browse");
        // Unknown stored modes read as sessions, never an error.
        let old: Layout = serde_json::from_str("{\"mode\":\"tabs\"}").unwrap();
        assert_eq!(old.mode, Mode::Sessions);
    }

    #[test]
    fn switching_saves_the_leaving_mode_and_restores_the_entering_one() {
        let mut slots = ModeSlots::default();
        let sessions_left = ModeState {
            sidebar_open: true,
            right_open: true,
            right_width: Some(420.0),
            right_kind: Some(RightKind::Files),
            scroll: Some(ScrollPos { item: 3, offset: 4.0 }),
            selected_session: Some("s-1".to_owned()),
            selected_tab: None,
        };
        // Into Browse for the first time: the sessions frame is saved,
        // and Browse restores its default (pane closed, nothing selected).
        let entering = slots.switch(Mode::Sessions, sessions_left.clone(), Mode::Browse);
        assert_eq!(entering, ModeState::default());
        assert_eq!(slots.get(Mode::Sessions), &sessions_left);
        // Back to Sessions: Browse's frame is saved, Sessions restores
        // exactly what it left with.
        let browse_left = ModeState {
            sidebar_open: false,
            right_open: false,
            right_width: None,
            right_kind: None,
            scroll: None,
            selected_session: None,
            selected_tab: Some(7),
        };
        let back = slots.switch(Mode::Browse, browse_left.clone(), Mode::Sessions);
        assert_eq!(back, sessions_left);
        assert_eq!(slots.get(Mode::Browse), &browse_left);
        // Untouched slots stay out of the file; remembered ones persist.
        assert!(ModeSlots::default().is_default_all());
        assert!(!slots.is_default_all());
        let text = serde_json::to_string(&slots).unwrap();
        let back: ModeSlots = serde_json::from_str(&text).unwrap();
        assert_eq!(back, slots);
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
        // Every in-memory spelling of the merged view writes `changes`.
        for kind in [RightKind::Changes, RightKind::Diff, RightKind::Git] {
            let text = serde_json::to_string(&kind).unwrap();
            assert_eq!(text, "\"changes\"", "{kind:?} writes as changes");
        }
        assert_eq!(serde_json::to_string(&RightKind::Files).unwrap(), "\"files\"");
        assert_eq!(serde_json::to_string(&RightKind::Browser).unwrap(), "\"browser\"");
        // The legacy variants label and slug as the view they load into.
        assert_eq!(RightKind::Diff.label(), "Changes");
        assert_eq!(RightKind::Git.label(), "Changes");
        assert_eq!(RightKind::Diff.slug(), "changes");
        assert_eq!(RightKind::Git.slug(), "changes");
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
}
