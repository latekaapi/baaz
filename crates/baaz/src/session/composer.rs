//! The composer's own surfaces: the `/`, `@`, model, effort and mode
//! menus, the prompt history the arrows walk, and pasted or attached images.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;
use crate::overlays::{EffortOption, EffortOptions, effort_detail, muse_efforts};

impl SessionView {
    // ------------------------------------------------------------ the menus

    /// The draft changed: re-derive the caret popovers and step off the
    /// history.
    pub(super) fn on_draft_changed(&mut self, cx: &mut Context<Self>) {
        self.note_draft(cx);
        if self.history.walking() {
            self.history.reset();
        }
        let (draft, caret) = self.draft_and_caret(cx);
        let token = caret_token(&draft, caret);
        self.overlays.update(cx, |overlays, _| {
            let caret_menu = matches!(
                overlays.menu.as_ref().map(|m| m.kind),
                Some(MenuKind::Command) | Some(MenuKind::Mention)
            );
            match token {
                Some((kind, at, filter)) => {
                    let same = overlays.menu.as_ref().is_some_and(|m| m.kind == kind && m.at == at);
                    if same {
                        if let Some(menu) = overlays.menu.as_mut() {
                            menu.filter = filter;
                            menu.selected = 0;
                        }
                    } else {
                        let mut menu = Menu::caret(kind, at);
                        menu.filter = filter;
                        overlays.open(menu);
                    }
                }
                None if caret_menu => overlays.menu = None,
                None => {}
            }
        });
        if let Some(filter) = self
            .overlays
            .read(cx)
            .menu
            .as_ref()
            .filter(|menu| menu.kind == MenuKind::Mention)
            .map(|menu| menu.filter.clone())
        {
            self.refresh_mentions(filter, cx);
        }
        cx.notify();
    }

    pub(super) fn draft_and_caret(&self, cx: &gpui::App) -> (String, usize) {
        let state = self.composer.read(cx);
        (state.value().to_string(), state.cursor())
    }

    /// Toggle the composer's `+` menu. Opening it closes any overlay menu
    /// first: the `+` menu is plain view state no overlay close ever sees,
    /// so without this the two would stack.
    pub fn toggle_plus_menu(&mut self, cx: &mut Context<Self>) {
        if self.plus_open {
            self.plus_open = false;
        } else {
            self.overlays.update(cx, |overlays, _| overlays.menu = None);
            self.plus_open = true;
        }
        cx.notify();
    }

    /// Open (or close) one of the chip pickers.
    pub fn toggle_picker(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let open = self.overlays.read(cx).is_open(kind);
        if open {
            self.overlays.update(cx, |overlays, _| overlays.menu = None);
            cx.notify();
            return;
        }
        // One menu at a time: the `+` menu is view-local, so the overlay
        // stack never saw it — close it before the picker opens.
        self.plus_open = false;
        let selected = match kind {
            MenuKind::Model => {
                self.load_models(cx);
                self.models.iter().position(|m| m.is_active).unwrap_or(0)
            }
            MenuKind::Effort => match self.effort_options() {
                EffortOptions::Unavailable(_) => 0,
                listed => {
                    listed.options().and_then(|options| {
                        options.iter().position(|option| option.effort == self.effort)
                    })
                    .unwrap_or(0)
                }
            },
            MenuKind::Mode => MODES.iter().position(|m| *m == self.mode()).unwrap_or(0),
            MenuKind::Provider => Self::provider_rows(self.provider_kind(), self.has_turns())
                .iter()
                .position(|row| row.id == self.provider_kind().as_str())
                .unwrap_or(0),
            _ => 0,
        };
        self.overlays.update(cx, |overlays, _| overlays.open(Menu::picker(kind, selected)));
        cx.notify();
    }

    /// How many rows the open menu has, which is what the arrow keys wrap on.
    /// An unanswerable catalog still counts its reason row: zero rows is a
    /// failure, not an empty state, and Enter must land somewhere that acts.
    pub fn menu_rows(&self, cx: &gpui::App) -> usize {
        let overlays = self.overlays.read(cx);
        match overlays.menu.as_ref().map(|m| m.kind) {
            Some(MenuKind::Model) if self.models.is_empty() && self.models_error.is_some() => 1,
            Some(MenuKind::Model) => self.models.len(),
            Some(MenuKind::Effort) => match self.effort_options().options() {
                Some(options) => options.len(),
                // The reason row, like the model picker's: zero rows is a
                // failure, not an empty state.
                None => 1,
            },
            Some(MenuKind::Mode) => MODES.len(),
            Some(MenuKind::Provider) => {
                Self::provider_rows(self.provider_kind(), self.has_turns()).len()
            }
            Some(MenuKind::Command) => {
                let filter = overlays.menu.as_ref().map(|m| m.filter.clone()).unwrap_or_default();
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                commands.len() + skill_rows.len()
            }
            Some(MenuKind::Mention) => {
                let filter = overlays.menu.as_ref().map(|m| m.filter.clone()).unwrap_or_default();
                self.mention_rows(&filter, cx).len()
            }
            // The shell's own menus are rendered and driven by the window,
            // not by the composer: no rows here, no Enter here.
            Some(MenuKind::Overflow | MenuKind::ViewOptions | MenuKind::Account | MenuKind::Project) => 0,
            None => 0,
        }
    }

    /// Enter on the open menu.
    pub fn confirm_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, selected, filter)) = self
            .overlays
            .read(cx)
            .menu
            .as_ref()
            .map(|m| (m.kind, m.selected, m.filter.clone()))
        else {
            return;
        };
        match kind {
            MenuKind::Model => {
                if let Some(model) = self.models.get(selected).map(|m| m.model_id.clone()) {
                    self.pick_model(&model, cx);
                } else if let Some(reason) = self.models_error.clone() {
                    // The reason row: restates why there is no catalog,
                    // then closes. Every row acts on click; none goes dead.
                    self.toast("Models unavailable", reason, cx);
                    self.close_menu(cx);
                }
            }
            MenuKind::Effort => {
                let listed = self.effort_options();
                if let Some(option) =
                    listed.options().and_then(|options| options.get(selected))
                {
                    let effort = option.effort;
                    self.pick_effort(effort, cx);
                } else if let EffortOptions::Unavailable(reason) = listed {
                    // The reason row: restates why there is no control,
                    // then closes. Every row acts on click; none goes dead.
                    self.toast("Effort unavailable", reason, cx);
                    self.close_menu(cx);
                }
            }
            MenuKind::Mode => {
                if let Some(mode) = MODES.get(selected).copied() {
                    self.pick_mode(mode, cx);
                }
            }
            MenuKind::Provider => {
                let rows = Self::provider_rows_for(
                    self.provider_kind(),
                    self.has_turns(),
                    &crate::settings_providers::live_visible_provider_ids(),
                );
                if let Some(row) = rows.get(selected) {
                    self.pick_provider(&row.id.clone(), cx);
                }
            }
            MenuKind::Command => {
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                if let Some(command) = commands.get(selected).copied() {
                    self.run_command(command, window, cx);
                } else if let Some(skill) = skill_rows.get(selected.saturating_sub(commands.len())) {
                    let insertion = format!("/{} ", skill.name);
                    self.replace_token(&insertion, window, cx);
                }
            }
            MenuKind::Mention => {
                let rows = self.mention_rows(&filter, cx);
                if let Some(path) = rows.get(selected).cloned() {
                    self.replace_token(&format!("@{path} "), window, cx);
                }
            }
            // Click-driven by the window; Enter stays with the composer.
            MenuKind::Overflow | MenuKind::ViewOptions | MenuKind::Account | MenuKind::Project => {}
        }
    }

    /// A click on a `/` menu row, which names itself rather than its index.
    pub(super) fn select_command(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = Command::parse(id.as_ref()) {
            self.run_command(command, window, cx);
            return;
        }
        let name = self
            .overlays
            .read(cx)
            .skills_for(&self.workspace)
            .iter()
            .find(|s| s.id == id.as_ref())
            .map(|s| s.name.clone());
        if let Some(name) = name {
            self.replace_token(&format!("/{name} "), window, cx);
        }
    }

    pub(super) fn pick_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        self.set_model(model_id, cx);
        cx.emit(SessionEvent::ModelSelected { model_id: model_id.to_owned() });
        self.close_menu(cx);
    }

    /// The effort menu's answer for this session: the current provider's
    /// levels for the currently selected model, or the typed reason there
    /// is no control. muse offers the whole closed enum, unchanged; Codex
    /// reads the selected model's `supportedReasoningEfforts` with the
    /// provider's own descriptions (or the catalog's union with a note when
    /// the model names no row); Claude Code reads the selected value's own
    /// `supportedEffortLevels`, offering only `Default` with a note before
    /// any catalog folds. Nothing here is a constant shared across
    /// providers: changing the model re-derives the list, so the chooser
    /// follows the selection.
    pub(super) fn effort_options(&self) -> EffortOptions {
        match self.provider_kind() {
            ProviderId::Muse => EffortOptions::Available(muse_efforts()),
            ProviderId::ClaudeCode => self.claude_code_efforts(),
            ProviderId::Codex => self.codex_effort_options(),
        }
    }

    /// The muted note the effort menu shows when the session's model names
    /// no catalog row: the levels are the catalog's union, offered so the
    /// person can steer, and the server validates the pick per turn.
    const CODEX_OFF_CATALOG_NOTE: &str = "Not in Codex's model list — Codex validates the level";

    /// The Claude Code twin of [`Self::CODEX_OFF_CATALOG_NOTE`]: a model
    /// no catalog row matches, even after normalising, still offers the
    /// levels every row supports (the intersection), with the muted note
    /// saying whose validation the levels carry.
    const CLAUDE_OFF_CATALOG_NOTE: &str =
        "Not in Claude Code's model list — Claude Code validates the level";

    /// What the Claude Code effort menu says before any catalog folds: the
    /// picker component has no disabled row, so the menu offers only
    /// `Default`, and this note rides its detail line saying when the
    /// levels arrive. The `--effort` launch flag's levels
    /// (`argv::CLAUDE_EFFORT_LEVELS`) stay argv validation only — never a
    /// menu offered as if the model validated them.
    const CLAUDE_PRE_CATALOG_NOTE: &str =
        "Effort levels load when the session starts — only Default until then";

    /// The level order a union follows: the closed enum's own spellings,
    /// least to most budget. A catalog id outside this list fails
    /// `parse_effort` at option build and is skipped, never fabricated.
    const CODEX_LEVEL_ORDER: [&str; 8] =
        ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

    /// Levels offered when no catalog folded at all: the middle of the
    /// range, the same default the owner's config names.
    const CODEX_EMPTY_CATALOG_LEVELS: [&str; 4] = ["low", "medium", "high", "xhigh"];

    /// The Codex arm of [`SessionView::effort_options`]: the selected
    /// model's levels out of the folded catalog, `Default` first. A level
    /// the closed enum cannot spell is skipped, never fabricated; a model
    /// with no row still offers the catalog's union with the muted note
    /// (the level rides `turn/start` and the server validates it — a
    /// rejection banners, never silences); a model whose row names no
    /// levels explains itself instead of opening an empty menu.
    fn codex_effort_options(&self) -> EffortOptions {
        let current = self
            .models
            .iter()
            .find(|m| m.is_active)
            .map(|m| m.model_id.as_str())
            .or(self.pending_model.as_deref());
        let Some(model) = current else {
            return EffortOptions::Unavailable(
                "No model is selected, so there is no catalog row to read reasoning levels from."
                    .to_owned(),
            );
        };
        let Some(levels) = self.codex_efforts.get(model) else {
            let union = Self::codex_union_levels(&self.codex_efforts);
            let mut options =
                vec![EffortOption { effort: None, detail: effort_detail(None).to_owned() }];
            options.extend(union.into_iter().filter_map(|id| {
                let effort = crate::projects::parse_effort(&id)?;
                Some(EffortOption {
                    effort: Some(effort),
                    detail: effort_detail(Some(effort)).to_owned(),
                })
            }));
            return EffortOptions::AvailableWithNote {
                options,
                note: Self::CODEX_OFF_CATALOG_NOTE.to_owned(),
            };
        };
        let mut options = vec![EffortOption { effort: None, detail: effort_detail(None).to_owned() }];
        options.extend(levels.iter().filter_map(|level| {
            let effort = crate::projects::parse_effort(&level.id)?;
            Some(EffortOption {
                effort: Some(effort),
                detail: level
                    .description
                    .clone()
                    .unwrap_or_else(|| effort_detail(Some(effort)).to_owned()),
            })
        }));
        if options.len() == 1 {
            return EffortOptions::Unavailable(format!(
                "{model} advertises no reasoning levels in its catalog row."
            ));
        }
        EffortOptions::Available(options)
    }

    /// The union of level ids every catalog row reports, in
    /// [`Self::CODEX_LEVEL_ORDER`] — or the middle-of-the-range fallback
    /// when no catalog folded at all.
    fn codex_union_levels(
        efforts: &std::collections::HashMap<String, Vec<provider_codex::child::SupportedEffort>>,
    ) -> Vec<String> {
        if efforts.is_empty() {
            return Self::CODEX_EMPTY_CATALOG_LEVELS.into_iter().map(str::to_owned).collect();
        }
        let mut union: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for levels in efforts.values() {
            for level in levels {
                union.insert(level.id.as_str());
            }
        }
        Self::CODEX_LEVEL_ORDER
            .into_iter()
            .filter(|level| union.contains(level))
            .map(str::to_owned)
            .collect()
    }

    /// Pick a reasoning effort. Applies to a session with turns exactly as
    /// to a fresh one — effort, like model, is switchable mid-session and
    /// rides the next turn — and the chip reads the pick at once.
    pub(super) fn pick_effort(&mut self, effort: Option<ReasoningEffort>, cx: &mut Context<Self>) {
        self.effort = effort;
        cx.emit(SessionEvent::EffortSelected { effort: crate::projects::effort_string(effort) });
        self.close_menu(cx);
    }

    /// The Claude Code arm of [`SessionView::effort_options`]: the selected
    /// catalog value's own `supportedEffortLevels`, `Default` first. A row
    /// that names no levels (Haiku, or `supportsEffort: false`) explains
    /// itself instead of listing levels; a resolved full id the menu never
    /// lists (the chip reads "Opus 5 · 1M" after a turn) first resolves to
    /// its catalog row by normalised id, and only a model no row matches
    /// offers the levels every row supports (the intersection); with no
    /// catalog folded yet the menu offers only `Default` with the note
    /// saying the levels load when the session starts. A level the closed
    /// enum cannot spell is skipped, never fabricated.
    fn claude_code_efforts(&self) -> EffortOptions {
        if self.claude_efforts.is_empty() {
            return EffortOptions::AvailableWithNote {
                options: vec![EffortOption {
                    effort: None,
                    detail: effort_detail(None).to_owned(),
                }],
                note: Self::CLAUDE_PRE_CATALOG_NOTE.to_owned(),
            };
        }
        let current = self
            .models
            .iter()
            .find(|m| m.is_active)
            .map(|m| m.model_id.as_str())
            .or(self.pending_model.as_deref());
        if let Some(levels) = current.and_then(|model| self.claude_efforts.get(model)) {
            return Self::claude_row_options(current.unwrap_or("this model"), levels);
        }
        if let Some(model) = current {
            if let Some(levels) = Self::resolve_claude_row(model, &self.claude_efforts) {
                return Self::claude_row_options(model, &levels);
            }
        }
        EffortOptions::AvailableWithNote {
            options: Self::claude_level_options(&Self::claude_intersection_levels(&self.claude_efforts)),
            note: Self::CLAUDE_OFF_CATALOG_NOTE.to_owned(),
        }
    }

    /// One Claude Code catalog row as a menu: its own levels, or the
    /// no-levels reason when the row names none.
    fn claude_row_options(model: &str, levels: &[String]) -> EffortOptions {
        if levels.is_empty() {
            return EffortOptions::Unavailable(format!(
                "{model} offers no reasoning levels, so there is no effort to set."
            ));
        }
        EffortOptions::Available(Self::claude_level_options(levels))
    }

    /// Resolve an off-catalog Claude Code model id to catalog levels: both
    /// sides normalize through `provider_claude_code::frame::normalize_model_id`
    /// (case, `[...]` context, `-YYYYMMDD` date, leading `claude-`). Rows that
    /// normalize to the same id win; failing that, rows of the same family
    /// (the first `-` segment), so a resolved `claude-opus-5[1m]` finds the
    /// `opus[1m]` row. When several rows match, only the levels every one of
    /// them supports are offered — never one picked by map order, which two
    /// same-family models (`opus`, `opus-4`) would make a coin toss.
    pub(super) fn resolve_claude_row(
        model: &str,
        efforts: &std::collections::HashMap<String, Vec<String>>,
    ) -> Option<Vec<String>> {
        use provider_claude_code::frame::normalize_model_id;
        let want = normalize_model_id(model);
        let family = |id: &str| id.split('-').next().unwrap_or(id).to_owned();
        let pick = |matches: std::collections::HashMap<String, Vec<String>>| match matches.len() {
            0 => None,
            1 => matches.into_values().next(),
            _ => Some(Self::claude_intersection_levels(&matches)),
        };
        let exact: std::collections::HashMap<String, Vec<String>> = efforts
            .iter()
            .filter(|(key, _)| normalize_model_id(key) == want)
            .map(|(key, levels)| (key.clone(), levels.clone()))
            .collect();
        pick(exact).or_else(|| {
            let want_family = family(&want);
            pick(
                efforts
                    .iter()
                    .filter(|(key, _)| family(&normalize_model_id(key)) == want_family)
                    .map(|(key, levels)| (key.clone(), levels.clone()))
                    .collect(),
            )
        })
    }

    /// The levels every catalog row supports, in launch-flag order: rows
    /// that name no levels offer no control at all, so only rows with
    /// levels intersect. An empty intersection is `Default` alone — still
    /// offered with the off-catalog note, never the hardcoded flag list.
    fn claude_intersection_levels(
        efforts: &std::collections::HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut sets: Vec<std::collections::HashSet<&str>> = efforts
            .values()
            .filter(|levels| !levels.is_empty())
            .map(|levels| levels.iter().map(String::as_str).collect())
            .collect();
        let Some(first) = sets.pop() else { return Vec::new() };
        let shared: std::collections::HashSet<&str> =
            sets.into_iter().fold(first, |acc, set| acc.intersection(&set).copied().collect());
        provider_claude_code::argv::CLAUDE_EFFORT_LEVELS
            .into_iter()
            .filter(|level| shared.contains(level))
            .map(str::to_owned)
            .collect()
    }

    /// One effort menu from catalog level ids, `Default` first, with the
    /// static detail lines (the catalog names levels, not descriptions).
    fn claude_level_options(levels: &[String]) -> Vec<EffortOption> {
        let mut options =
            vec![EffortOption { effort: None, detail: effort_detail(None).to_owned() }];
        // The catalog answers in budget order already; keep it, skipping
        // what the closed enum cannot spell.
        for level in levels {
            if let Some(effort) = crate::projects::parse_effort(level) {
                // `Default` is the menu's first row, never a level row.
                if options.iter().any(|option| option.effort == Some(effort)) {
                    continue;
                }
                options.push(EffortOption {
                    effort: Some(effort),
                    detail: effort_detail(Some(effort)).to_owned(),
                });
            }
        }
        options
    }

    /// The session's project display name, for the empty state. Synced by
    /// the window on every activation, so a rename lands without reopening.
    pub(crate) fn set_project_name(&mut self, name: Option<String>) {
        self.project_name = name;
    }

    /// A new session's starting effort, from its project's defaults: applied
    /// before the first turn, reporting nothing back — it already is what
    /// the project says.
    pub(crate) fn set_initial_effort(&mut self, effort: Option<ReasoningEffort>, cx: &mut Context<Self>) {
        self.effort = effort;
        cx.notify();
    }

    pub(super) fn pick_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        self.set_mode(mode, cx);
        cx.emit(SessionEvent::ModeSelected { mode: wire_mode(mode) });
        self.close_menu(cx);
    }

    /// A click (or Enter) on a provider menu row. A `handoff:<wire>` row
    /// starts a handoff to that backend (a lossy re-prompt, never a lane
    /// swap); a plain wire id keeps the old contract. Picking the
    /// session's own provider just closes the menu — reopening the lane
    /// it already rides is not a swap, and a same-provider pick is a
    /// model change, never a handoff. A fresh session swaps through the
    /// application (nothing sent, nothing to keep); a session with turns
    /// keeps its lane and the plain pick starts a new session on the
    /// other backend instead.
    pub(super) fn pick_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(rest) = id.strip_prefix("handoff:") {
            let picked = ProviderId::parse(rest);
            if picked == self.provider_kind() {
                self.close_menu(cx);
                return;
            }
            cx.emit(SessionEvent::HandoffRequested { provider: picked, headless: false });
            self.close_menu(cx);
            return;
        }
        let picked = ProviderId::parse(id);
        if picked == self.provider_kind() {
            self.close_menu(cx);
            return;
        }
        // A disabled provider left the menu, but a scripted verb can still
        // name it: refuse with the way back instead of switching.
        if !crate::settings_providers::live_visible_provider_ids().contains(&picked) {
            self.toast(
                format!("{} is disabled", picked.label()),
                "Enable it in Settings → Providers first.",
                cx,
            );
            self.close_menu(cx);
            return;
        }
        if self.has_turns() {
            cx.emit(SessionEvent::NewSessionOnProvider { provider: picked });
        } else {
            cx.emit(SessionEvent::SwitchProvider { provider: picked });
        }
        self.close_menu(cx);
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.menu = None);
        cx.notify();
    }

    /// Close the composer's view-local `+` menu, if open. The chip pickers
    /// and caret popovers live in the shared overlay stack (closed by
    /// `close_topmost`), but the `+` menu is plain view state no overlay
    /// close ever sees — so Escape's `cancel` has to ask the view (V1).
    /// Reports whether anything closed, like `close_topmost` does.
    pub fn close_plus_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.plus_open {
            return false;
        }
        self.plus_open = false;
        cx.notify();
        true
    }

    /// Whether the `+` menu stands open: the Escape test, and the browser
    /// pane's cover check (a native view cannot sit under a gpui popover).
    pub(crate) fn plus_open(&self) -> bool {
        self.plus_open
    }

    /// The provider menu's rows for a session on `current`.
    ///
    /// A fresh session offers the three backends as a swap. A session
    /// with turns keeps its lane, so every other backend gets two rows:
    /// "Hand off to X…" first (a `handoff:<wire>` id, starting the
    /// lossy re-prompt), then "New session on X" with the reason in the
    /// detail line. The picker's rows always act on click — the typed
    /// reason rides as prose, the way an `Unavailable` capability
    /// explains itself — and row ids are what [`Self::pick_provider`]
    /// parses back; only the labels differ.
    pub(super) fn provider_rows(current: ProviderId, has_turns: bool) -> Vec<ProviderRow> {
        Self::provider_rows_for(current, has_turns, &ProviderId::all())
    }

    /// [`Self::provider_rows`] over `ids`: the live menu passes only the
    /// enabled providers, so a disabled provider disappears from the
    /// composer's provider menu (and from the new-session provider
    /// choice, which opens the same menu).
    pub(super) fn provider_rows_for(
        current: ProviderId,
        has_turns: bool,
        ids: &[ProviderId],
    ) -> Vec<ProviderRow> {
        let mut rows = Vec::new();
        // The session's own lane always stays, even switched off mid-run:
        // its rows explain the lane it already rides.
        let mut ids: Vec<ProviderId> = ids.to_vec();
        if !ids.contains(&current) {
            ids.push(current);
        }
        ids.sort_by_key(|id| ProviderId::all().iter().position(|all| all == id).unwrap_or(99));
        for id in ids {
            if has_turns && id != current {
                rows.push(ProviderRow {
                    id: format!("handoff:{}", id.as_str()),
                    label: format!("Hand off to {}…", id.label()),
                    detail: format!(
                        "Start a fresh {} session with a summary of this one. Tool state, pending approvals and provider memory do not carry over.",
                        id.label()
                    ),
                });
                rows.push(ProviderRow {
                    id: id.as_str().to_owned(),
                    label: format!("New session on {}", id.label()),
                    detail: format!(
                        "This session already has turns on {}, so its provider cannot be switched. Starts a new session on {}.",
                        current.label(),
                        id.label()
                    ),
                });
            } else {
                rows.push(ProviderRow {
                    id: id.as_str().to_owned(),
                    label: id.label().to_owned(),
                    detail: id.blurb().to_owned(),
                });
            }
        }
        rows
    }

    /// Run one client-side slash command (spec §3.10).
    ///
    /// `argument` is whatever followed the command when it was typed; the `/`
    /// menu always passes an empty one, because a menu row carries no text.
    pub fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.run_command_with(command, String::new(), window, cx);
    }

    /// [`SessionView::run_command`] with whatever followed the command.
    pub(crate) fn run_command_with(&mut self, command: Command, argument: String, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_token("", window, cx);
        match command {
            Command::Model => self.toggle_picker(MenuKind::Model, cx),
            Command::Effort => self.toggle_picker(MenuKind::Effort, cx),
            Command::Mode => self.toggle_picker(MenuKind::Mode, cx),
            Command::Plan => {
                let on = !self.plan;
                self.set_plan(on, cx);
            }
            Command::Compact => self.compact(cx),
            Command::Clear => cx.emit(SessionEvent::NewSession),
            // `/handoff` opens the provider picker on the handoff rows: a
            // session with turns offers "Hand off to X…" there. With no
            // turns there is nothing to carry, so it says so instead of
            // opening a menu whose handoff rows would all refuse.
            Command::Handoff => {
                if self.has_turns() {
                    self.toggle_picker(MenuKind::Provider, cx);
                } else {
                    self.set_banner("Nothing to hand off yet — send a turn first.", None, cx);
                }
            }
            Command::Logout => cx.emit(SessionEvent::Logout),
            Command::Status | Command::Usage => {
                let detail = self.status_text(cx);
                cx.emit(SessionEvent::Status { detail });
            }
            Command::Help => {
                self.overlays.update(cx, |overlays, _| overlays.open(Menu::caret(MenuKind::Command, 0)));
                cx.notify();
            }
            // `/fork` with nothing named opens the turn picker; `/fork <n>`
            // forks the nth newest completed turn with no picker in between.
            Command::Fork => {
                let argument = argument.trim();
                if argument.is_empty() {
                    cx.emit(SessionEvent::ForkPicker);
                } else {
                    match argument.parse::<usize>() {
                        Ok(n) => self.fork_nth(n, cx),
                        Err(_) => self.set_banner("`/fork` takes a turn number, e.g. `/fork 2`.", None, cx),
                    }
                }
            }
            // `/name Fix the parser` renames; `/name` on its own opens the
            // row's field, and `/name ` with nothing after it clears the name.
            Command::Name => {
                let text = argument.trim();
                match (text.is_empty(), argument.is_empty()) {
                    (true, true) => cx.emit(SessionEvent::RenameStart),
                    (true, false) => cx.emit(SessionEvent::Rename { name: None }),
                    _ => cx.emit(SessionEvent::Rename { name: Some(text.to_owned()) }),
                }
            }
            Command::Hide => cx.emit(SessionEvent::Hide),
            Command::Empty => cx.emit(SessionEvent::ToggleEmpty),
            Command::Resume => cx.emit(SessionEvent::Resume),
            Command::Search => cx.emit(SessionEvent::Search),
            Command::Project => cx.emit(SessionEvent::Projects),
            // Window-level commands: the session cannot act on the window, so
            // it hands them up. They reach the same `run_window_command` the
            // ⌘K palette uses, which is what keeps the two routes honest —
            // these also appear in the composer's own `/` menu (it is
            // built from `Command::ALL`), and a menu row that did nothing
            // would be worse than no row. `/skills` typed as a whole draft
            // arrives here too: the page opens, and no turn is ever sent.
            Command::RightBrowser
            | Command::RightDiff
            | Command::RightGit
            | Command::RightFiles
            | Command::Terminal
            | Command::Skills
            | Command::NewTerminalCmd => cx.emit(SessionEvent::WindowCommand(command)),
        }
    }

    /// The `/status` and `/usage` body: everything the session knows about
    /// itself, from the fold rather than from what the app last sent.
    pub(super) fn status_text(&self, cx: &gpui::App) -> String {
        let context = self.context();
        let branch = self.session().and_then(|s| s.branch.clone()).unwrap_or_else(|| "—".to_owned());
        let queued = self.fold.side(&self.session_id).map(|s| s.queued.len()).unwrap_or(0);
        let _ = cx;
        format!(
            "Model: {}\nApproval mode: {}\nReasoning effort: {}\nPlan mode: {}\nContext: {}\nSession tokens: {} prompt · {} output · {} total\nQueued: {queued}\nSession: {}\nWorkspace: {}\nBranch: {branch}",
            self.model(),
            self.mode_label(),
            crate::overlays::effort_label(self.effort),
            if self.plan { "on" } else { "off" },
            context.label(),
            context.prompt_tokens,
            context.output_tokens,
            context.total_tokens,
            self.session_id,
            self.workspace,
        )
    }

    /// Replace the `/…` or `@…` token the caret is in with `insertion`.
    pub(super) fn replace_token(&mut self, insertion: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.overlays.read(cx).menu.as_ref().map(|m| m.at) else { return };
        let (draft, caret) = self.draft_and_caret(cx);
        if at > draft.len() || caret > draft.len() || at > caret {
            self.close_menu(cx);
            return;
        }
        let mut next = String::with_capacity(draft.len() + insertion.len());
        next.push_str(&draft[..at]);
        next.push_str(insertion);
        next.push_str(&draft[caret..]);
        let offset = at + insertion.len();
        let position = position_of(&next, offset);
        self.composer.update(cx, |state, cx| {
            state.set_value(next, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        self.note_draft(cx);
        self.close_menu(cx);
    }

    // ---------------------------------------------------------------- history

    /// ↑ on the first line of the draft.
    pub fn history_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if let Some(text) = self.history.prev(&draft) {
            self.set_history_value(text, window, cx);
        }
    }

    /// ↓ on the last line of the draft.
    pub fn history_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.history.next() {
            self.set_history_value(text, window, cx);
        }
    }

    /// Whether the caret is on the draft's first (or last) line, which is what
    /// arms ↑ and ↓ for the history rather than for the editor.
    pub fn caret_edges(&self, cx: &gpui::App) -> (bool, bool) {
        let state = self.composer.read(cx);
        let value = state.value();
        let line = state.cursor_position().line;
        let last = value.matches('\n').count() as u32;
        (line == 0, line >= last)
    }

    /// Setting the value fires `InputEvent::Change`, which would reset the
    /// cursor the walk depends on, so the walk's own writes are marked.
    pub(super) fn set_history_value(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let position = position_of(&text, text.len());
        self.composer.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        self.note_draft(cx);
        cx.notify();
    }

    // ----------------------------------------------------------------- images

    /// ⌘V with an image on the clipboard.
    ///
    /// Returns `false` when the clipboard holds no image, so the caller can let
    /// the textarea's own paste have the keystroke.
    pub fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let mut pasted = false;
        for entry in item.into_entries() {
            if let ClipboardEntry::Image(image) = entry {
                self.attach_bytes("pasted", image.bytes.clone(), cx);
                pasted = true;
            }
        }
        pasted
    }

    /// The `+` menu's "Attach file or photo", and the drop of files from
    /// Finder. Image extensions attach as images; everything else is extracted
    /// to text by `attachments` (MSP has no file part to carry the bytes).
    pub fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let ext =
                path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                self.image_seq += 1;
                let id = format!("img-{}", self.image_seq);
                // The chip goes up now; the read, the decode and the
                // thumbnail happen on the background executor and replace it
                // when they land (finding `performance-14`). A ten-megabyte
                // photo used to stall the frame it was dropped on.
                self.images.push(images::placeholder(id.clone(), images::display_name(&path)));
                let work_id = id.clone();
                self.wire_call(
                    cx,
                    move || images::from_path(work_id, &path),
                    move |this: &mut Self, decoded, cx| this.resolve_image(&id, decoded, cx),
                );
                continue;
            }
            if self.files.len() >= attachments::MAX_FILES {
                self.banner = Some(format!(
                    "at most {} files per turn; the rest were not attached",
                    attachments::MAX_FILES
                ));
                continue;
            }
            self.file_seq += 1;
            match attachments::from_path(format!("file-{}", self.file_seq), &path) {
                Ok(file) => self.files.push(file),
                Err(reason) => self.banner = Some(reason),
            }
        }
        cx.notify();
    }

    pub(super) fn attach_bytes(&mut self, name: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.image_seq += 1;
        let id = format!("img-{}", self.image_seq);
        let chip_name = if name.is_empty() { "pasted image".to_owned() } else { name.to_owned() };
        // Same split as a dropped file (finding `performance-14`): the
        // clipboard already handed over the bytes, but the decode and the
        // thumbnail are the expensive half and they do not belong on a frame.
        self.images.push(images::placeholder(id.clone(), chip_name));
        let (work_id, name) = (id.clone(), name.to_owned());
        self.wire_call(
            cx,
            move || images::from_bytes(work_id, name, &bytes),
            move |this: &mut Self, decoded, cx| this.resolve_image(&id, decoded, cx),
        );
        cx.notify();
    }

    /// A background decode landed: swap the placeholder chip for the image, or
    /// take it away and say why.
    ///
    /// A chip the person removed while the decode ran is simply gone, and the
    /// result is dropped with it — removing a chip means removing it.
    pub(super) fn resolve_image(&mut self, id: &str, decoded: Result<images::Image, String>, cx: &mut Context<Self>) {
        let Some(slot) = self.images.iter().position(|image| image.id == id) else { return };
        match decoded {
            Ok(image) => self.images[slot] = image,
            Err(reason) => {
                self.images.remove(slot);
                self.banner = Some(reason);
            }
        }
        cx.notify();
    }

    /// Whether an attachment is still being read and decoded, which is what
    /// keeps the send button down until it lands (finding `performance-14`).
    pub fn attachments_pending(&self) -> bool {
        self.images.iter().any(|image| image.pending)
    }

    /// Z7a: attach already-held screenshot bytes (a browser page capture) as
    /// an image chip. The existing image path owns the decode off the frame;
    /// this is its entry point for bytes that never touched the filesystem.
    pub(crate) fn attach_screenshot(&mut self, name: String, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.attach_bytes(&name, bytes, cx);
    }

    /// Z7a: append a text block (browser annotations) to the draft without
    /// clobbering what is already typed. An empty draft takes the block as
    /// is; otherwise a blank line separates the two.
    pub(crate) fn append_draft_block(
        &mut self,
        block: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.draft_text(cx);
        let next = if current.trim().is_empty() {
            block
        } else {
            format!("{}\n\n{block}", current.trim_end())
        };
        self.set_draft(next, window, cx);
        self.on_draft_changed(cx);
    }

    /// Open the system picker for a file. Image extensions attach as images;
    /// everything else is extracted to text, so the prompt accepts any file.
    pub fn prompt_for_image(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update(cx, |this, cx| this.attach_paths(paths, cx));
        }));
    }

    /// Type a menu sigil (`@` or `/`) into the draft so its caret menu opens:
    /// what the `+` menu's Mention and commands rows do. The text goes through
    /// `set_draft`, so the caret lands at the end and the popover opens exactly
    /// as if the person had typed it.
    pub(super) fn insert_sigil(&mut self, sigil: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, _) = self.draft_and_caret(cx);
        let mut next = draft;
        if !next.is_empty() && !next.ends_with(char::is_whitespace) {
            next.push(' ');
        }
        next.push_str(sigil);
        self.set_draft(next, window, cx);
        self.on_draft_changed(cx);
        self.focus_composer(window, cx);
    }
}

/// One provider menu row: the id the pick parses back, the label the row
/// shows, and the detail line under it.
pub(super) struct ProviderRow {
    /// The pick id: a plain wire id, or `handoff:<wire>` for the handoff row.
    pub id: String,
    /// The row's label.
    pub label: String,
    /// The row's detail line.
    pub detail: String,
}
