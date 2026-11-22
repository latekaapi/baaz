//! The window's preferences as a gpui global ([`LayoutStore`]), and the
//! parts of `layout.json` that name the aui library: the theme, density and
//! text-size tokens, the window appearance, and the shell's width clamps.
//! The file's model (every field, its defaults, the store) is
//! [`baaz_core::layout`], re-exported here so `crate::layout::…` names both.

pub use baaz_core::layout::*;

/// A [`ThemePref`] as the library's theme.
pub trait ThemeToken {
    /// The theme this choice paints with while the system's appearance is
    /// `system_dark`: the seam System follows the OS through. Pure, so the
    /// tests drive both appearances without a window.
    fn resolve(self, system_dark: bool) -> aui_tokens::ThemeKind;
}

impl ThemeToken for ThemePref {
    fn resolve(self, system_dark: bool) -> aui_tokens::ThemeKind {
        match self {
            ThemePref::System if system_dark => aui_tokens::ThemeKind::Dark,
            ThemePref::System => aui_tokens::ThemeKind::Light,
            ThemePref::Light => aui_tokens::ThemeKind::Light,
            ThemePref::Dark => aui_tokens::ThemeKind::Dark,
        }
    }
}

/// A [`DensityPref`] as the library's density.
pub trait DensityToken: Sized {
    /// The library's density for this choice.
    fn density(self) -> aui_tokens::Density;

    /// The control's word for the choice: the library's own.
    fn label(self) -> &'static str {
        self.density().label()
    }
}

impl DensityToken for DensityPref {
    fn density(self) -> aui_tokens::Density {
        match self {
            DensityPref::Comfortable => aui_tokens::Density::Comfortable,
            DensityPref::Compact => aui_tokens::Density::Compact,
        }
    }
}

/// A [`TextSizePref`] as the library's text size.
pub trait TextSizeToken: Sized {
    /// The library's text size for this choice.
    fn text_size(self) -> aui_tokens::TextSize;

    /// The control's word for the choice: the library's own.
    fn label(self) -> &'static str {
        self.text_size().label()
    }
}

impl TextSizeToken for TextSizePref {
    fn text_size(self) -> aui_tokens::TextSize {
        match self {
            TextSizePref::Default => aui_tokens::TextSize::Default,
            TextSizePref::Large => aui_tokens::TextSize::Large,
        }
    }
}

/// Whether gpui's window appearance is a dark one (`darkAqua` or the
/// vibrant dark variant).
pub fn appearance_is_dark(appearance: gpui::WindowAppearance) -> bool {
    matches!(appearance, gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark)
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

/// [`Layout`] as the window's live preferences ([`LayoutStore`]): the read,
/// the edit and the one write path. A trait because the model lives in the
/// core crate; callers keep writing `Layout::get(cx)`.
pub trait LayoutGlobal {
    /// Read `layout.json` into the global, once.
    fn init(cx: &mut gpui::App);
    /// The live preferences.
    fn get(cx: &gpui::App) -> &Layout;
    /// Change the preferences and write `layout.json`.
    fn update<R>(cx: &mut gpui::App, edit: impl FnOnce(&mut Layout) -> R) -> R;
    /// Change the preferences without writing yet.
    fn stage<R>(cx: &mut gpui::App, edit: impl FnOnce(&mut Layout) -> R) -> R;
    /// Write the live preferences.
    fn save(cx: &gpui::App);
}

impl LayoutGlobal for Layout {
    /// Read `layout.json` into the global, once: a second call keeps the live
    /// value. Boot calls it before anything reads a preference (`main`), and
    /// [`crate::app::Harness::new`] calls it for test windows.
    fn init(cx: &mut gpui::App) {
        if !cx.has_global::<LayoutStore>() {
            let layout = read();
            crate::browser::agents::set_current(&layout.agents);
            cx.set_global(LayoutStore(layout));
        }
    }

    /// The live preferences.
    fn get(cx: &gpui::App) -> &Layout {
        cx.try_global::<LayoutStore>().map_or(&DEFAULT_LAYOUT, |store| &store.0)
    }

    /// Change the preferences and write `layout.json`: the one write path.
    /// Observers of [`LayoutStore`] re-render on the change. The edit gets
    /// the layout only, never `cx`, so it cannot re-enter the global.
    fn update<R>(cx: &mut gpui::App, edit: impl FnOnce(&mut Layout) -> R) -> R {
        let result = Self::stage(cx, edit);
        Self::save(cx);
        result
    }

    /// Change the preferences without writing yet: the next
    /// [`Layout::update`] (or the quit's [`Layout::save`]) carries it. For a
    /// session switch restoring that session's pane, which must not pay for
    /// a file write, and for scripted steps and tests.
    fn stage<R>(cx: &mut gpui::App, edit: impl FnOnce(&mut Layout) -> R) -> R {
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
    fn save(cx: &gpui::App) {
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

/// One drag move on the right pane's divider. The minus is the whole
/// difference from [`drag_width`]: the sidebar's divider sits on the
/// sidebar's RIGHT edge, so moving the pointer right widens it, while the
/// right pane's divider sits on the pane's LEFT edge, so moving the pointer
/// right NARROWS it.
pub fn right_drag_width(start_w: f32, grab_x: f32, x: f32) -> f32 {
    aui::shell::clamp_right_width(start_w - (x - grab_x))
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
            terminal_open: true,
            terminal_height: Some(300.0),
            terminal_place: TerminalPlace::Pane,
            right_open: true,
            right_width: Some(420.0),
            right_kind: Some(RightKind::Changes),
            right_lower: Some(RightKind::Terminal),
            right_split: Some(0.4),
            right_previous: Some(RightKind::Files),
            use_own_mcp: UseOwnMcp { codex: true, claude_code: false },
            mode: Mode::Browse,
            sidebar_open: false,
            sessions_pane: Some(SessionsPane { open: true, kind: Some(RightKind::Files), lower: None, split: None }),
            search_engine: crate::browser::toolbar::SearchEngine::Kagi,
            download_dir: Some(PathBuf::from("/tmp/baaz-downloads")),
            code_wrap: false,
            assistant: Default::default(),
            agents: Default::default(),
            adblock: Default::default(),
            worktree_branch_prefix: Some("lk/".into()),
            archive_on_merge: true,
            open_in: Some("zed".into()),
            onboarding: Onboarding { regions: Once::Done, default_browser: Once::Due },
        };
        let text = serde_json::to_string(&stored).unwrap();
        assert!(text.contains("\"worktreeBranchPrefix\":\"lk/\""));
        assert!(text.contains("\"groupBy\":\"project\""));
        assert!(text.contains("\"closedGroups\":[\"other\"]"));
        assert!(text.contains("\"expandedGroups\":[\"p-baaz\"]"));
        assert!(text.contains("\"searchAllProjects\":false"));
        assert!(text.contains("\"groupBar\":true"));
        assert!(text.contains("\"summaryLine\":true"));
        assert!(text.contains("\"theme\":\"system\""));
        assert!(text.contains("\"groupBranch\":true"));
        assert!(text.contains("\"foldFinishedTurns\":false"));
        assert!(text.contains("\"terminalOpen\":true"));
        assert!(text.contains("\"terminalHeight\":300.0"));
        assert!(text.contains("\"rightOpen\":true"));
        assert!(text.contains("\"rightWidth\":420.0"));
        assert!(text.contains("\"rightKind\":\"changes\""));
        assert!(text.contains("\"terminalPlace\":\"pane\""));
        assert!(text.contains("\"rightLower\":\"terminal\""));
        assert!(text.contains("\"rightSplit\":0.4"));
        assert!(text.contains("\"useOwnMcp\":{\"codex\":true,\"claudeCode\":false}"));
        assert!(text.contains("\"mode\":\"browse\""));
        assert!(text.contains("\"sidebarOpen\":false"));
        assert!(text.contains("\"sessionsPane\":{\"open\":true,\"kind\":\"files\"}"));
        assert!(text.contains("\"searchEngine\":\"kagi\""));
        assert!(text.contains("\"downloadDir\":\"/tmp/baaz-downloads\""));
        assert!(text.contains("\"archiveOnMerge\":true"));
        assert!(text.contains("\"onboarding\":{\"regions\":\"done\",\"defaultBrowser\":\"due\"}"));
        assert!(text.contains("\"openIn\":\"zed\""));
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert!(back.archive_on_merge);
        assert_eq!(back.open_in.as_deref(), Some("zed"));
        assert_eq!(back.group_by, Some(GroupBy::Project));
        assert_eq!(back.closed_groups, vec!["other".to_owned()]);
        assert_eq!(back.expanded_groups, vec!["p-baaz".to_owned()]);
        assert!(!back.search_all_projects);
        assert!(back.group_bar);
        assert!(back.summary_line);
        assert_eq!(back.theme, ThemePref::System);
        assert!(back.group_branch);
        assert!(!back.fold_finished_turns);
        assert!(back.terminal_open);
        assert_eq!(back.terminal_height, Some(300.0));
        assert!(back.right_open);
        assert_eq!(back.right_width, Some(420.0));
        assert_eq!(back.right_kind, Some(RightKind::Changes));
        assert_eq!(back.terminal_place, TerminalPlace::Pane);
        assert_eq!((back.right_lower, back.right_split), (Some(RightKind::Terminal), Some(0.4)));
        assert_eq!(back.right_previous, None, "the previous view is this window's, never stored");
        assert!(back.use_own_mcp.codex);
        assert!(!back.use_own_mcp.claude_code);
        assert_eq!(back.mode, Mode::Browse);
        assert!(!back.sidebar_open);
        assert_eq!(back.sessions_pane, Some(SessionsPane { open: true, kind: Some(RightKind::Files), lower: None, split: None }));
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
        // The retired `feedbackMode` key reads as nothing too
        // (`baaz_core::layout`'s `a_stored_feedback_mode_key_still_loads`).
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
