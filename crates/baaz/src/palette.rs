//! The command palette: ⌘K and the lists it shows — `/resume`, `/fork`, the
//! full-text search and the Projects palette — as one [`Picker`] view
//! (plan 4.6).
//!
//! The palette is the same gesture for every list (a list, an arrow key and
//! a return), so it is one picker with one delegate, [`PaletteDelegate`].
//! The root pushes the rows in each frame it draws ([`PaletteRows`], read
//! from the sessions, the projects, the search results it keeps) and maps
//! what the palette hands back ([`PalettePick`]) onto its own work
//! (`Harness::run_palette_row`); the selection, the Commands and Projects
//! query fields and the keyboard are the palette's own, so typing and the
//! arrows redraw the palette alone.

use std::path::PathBuf;

use aui::nav::folder_drop_card;
use aui::overlay::{command_palette, popover_layer, PaletteIcon, PaletteItem, PaletteSection};
use aui_icons::IconName;
use aui_tokens::scale;
use aui::interact::dismiss_layer;
use aui::util::v_flex;
use gpui::{div, prelude::*, AnyElement, App, Context, Entity, Focusable as _, SharedString, Window};

use crate::app::Harness;
use crate::overlay_stack::{Entry, Layer, LayerKind};
use crate::overlays::{Command, Palette, PaletteKind};
use crate::picker::{Picked, Picker, PickerDelegate, PickerFrame};

/// The palette view.
pub(crate) type PaletteView = Picker<PaletteDelegate>;

/// What the palette hands its owner.
#[derive(Clone, Debug)]
pub(crate) enum PalettePick {
    /// A row, by its id: the owner runs it for the palette's kind.
    Row(SharedString),
    /// The Projects palette's folder card (or ↩ on an empty list): choose a
    /// folder through the system panel.
    ChooseFolder,
    /// Directories dropped on the Projects palette's folder card.
    Drop(Vec<PathBuf>),
}

/// One candidate of the Projects list, before the query narrows it.
#[derive(Clone, Debug)]
pub(crate) struct ProjectRow {
    /// `p:<project id>` or `a:<workspace root>`.
    pub(crate) id: SharedString,
    /// In the Add section (a recent workspace) rather than Projects.
    pub(crate) add: bool,
    /// The row's label: the project's name, or the workspace's folder name.
    pub(crate) name: String,
    /// The path the query matches against.
    pub(crate) path: String,
    /// The muted context: the root, `~` for home on adopted projects.
    pub(crate) context: String,
    /// The visible session count badge.
    pub(crate) count: usize,
}

/// How many recent workspaces the Add section lists.
const RECENT_WORKSPACES: usize = 12;

/// What the palette lists, pushed by the root.
pub(crate) enum PaletteRows {
    /// ⌘K: every command the open session's provider offers (every command
    /// with no session open), narrowed by the query field.
    Commands(Vec<Command>),
    /// `/resume` and `/fork`: id, label, detail — no query.
    Plain(Vec<(SharedString, SharedString, SharedString)>),
    /// The search palette: session hits then file hits (id, label, detail,
    /// the project badge), and the status line its query row shows.
    Search { rows: Vec<(SharedString, SharedString, SharedString, Option<SharedString>)>, status: SharedString },
    /// The Projects palette's candidates, narrowed by its query field.
    Projects(Vec<ProjectRow>),
}

/// The palette's delegate: what it lists and how it draws.
pub(crate) struct PaletteDelegate {
    /// Which list.
    pub(crate) kind: PaletteKind,
    rows: PaletteRows,
    /// The query the rows are narrowed to.
    query: String,
    /// The candidate rows the query leaves, in order.
    matches: Vec<usize>,
    /// Drawn at rest (a scripted capture): no enter presence.
    still: bool,
}

impl PaletteDelegate {
    /// A palette on `kind` listing `rows`.
    pub(crate) fn new(kind: PaletteKind, rows: PaletteRows, still: bool) -> Self {
        let mut delegate = PaletteDelegate { kind, rows, query: String::new(), matches: Vec::new(), still };
        delegate.update_matches("");
        delegate
    }

    /// Replace the rows (the root's push).
    pub(crate) fn set_rows(&mut self, rows: PaletteRows) {
        self.rows = rows;
    }

    /// Every listed row's id, in order: what row `n` is to the keyboard and
    /// the click alike.
    pub(crate) fn ids(&self) -> Vec<SharedString> {
        (0..self.match_count()).filter_map(|ix| self.match_id(ix)).collect()
    }

    /// The search status the query row shows.
    pub(crate) fn status(&self) -> Option<&SharedString> {
        match &self.rows {
            PaletteRows::Search { status, .. } => Some(status),
            _ => None,
        }
    }

    /// The row `ix` of the matches: id, label, detail, badge, icon.
    fn row(&self, ix: usize) -> Option<(SharedString, SharedString, SharedString, Option<SharedString>, PaletteIcon)> {
        let at = *self.matches.get(ix)?;
        let glyph = |name| PaletteIcon::Glyph(name);
        Some(match &self.rows {
            PaletteRows::Commands(commands) => {
                let command = commands.get(at)?;
                (command.slash().into(), command.slash().into(), command.description().into(), None, glyph(IconName::Slash))
            }
            PaletteRows::Plain(rows) => {
                let (id, label, detail) = rows.get(at)?.clone();
                let icon = if self.kind == PaletteKind::Fork { IconName::Git } else { IconName::Clock };
                (id, label, detail, None, glyph(icon))
            }
            PaletteRows::Search { rows, .. } => {
                let (id, label, detail, badge) = rows.get(at)?.clone();
                let icon = if id.starts_with("s:") { IconName::Clock } else { IconName::File };
                (id, label, detail, badge, glyph(icon))
            }
            PaletteRows::Projects(rows) => {
                let row = rows.get(at)?;
                (row.id.clone(), row.name.clone().into(), row.context.clone().into(), Some(row.count.to_string().into()), glyph(IconName::Folder))
            }
        })
    }

    /// Whether row `ix` of the matches is a recent workspace (Add section).
    fn adds(&self, ix: usize) -> bool {
        match &self.rows {
            PaletteRows::Projects(rows) => self.matches.get(ix).and_then(|at| rows.get(*at)).is_some_and(|row| row.add),
            _ => false,
        }
    }
}

/// The Commands palette's filter: the commands narrowed to the query,
/// matching the slash and the description case-insensitively. The empty
/// query lists every command.
fn command_matches(commands: &[Command], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    commands
        .iter()
        .enumerate()
        .filter(|(_, command)| {
            needle.is_empty()
                || command.slash().to_lowercase().contains(&needle)
                || command.description().to_lowercase().contains(&needle)
        })
        .map(|(at, _)| at)
        .collect()
}

/// The Commands palette's rows for `query` on `provider`'s sessions (every
/// command with none open): [`command_matches`] over the offered commands.
#[cfg(feature = "probe")]
#[cfg(test)]
pub(crate) fn filter_commands(query: &str, provider: Option<crate::providers::ProviderId>) -> Vec<Command> {
    let offered: Vec<Command> = Command::ALL.into_iter().filter(|command| provider.is_none_or(|id| command.offered_on(id))).collect();
    command_matches(&offered, query).into_iter().map(|at| offered[at]).collect()
}

/// The Projects palette's filter: both sections narrowed by name and path,
/// the Add section to the first [`RECENT_WORKSPACES`] that match.
fn project_matches(rows: &[ProjectRow], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    let mut recents = 0;
    let mut out = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let matched = needle.is_empty() || row.name.to_lowercase().contains(&needle) || row.path.to_lowercase().contains(&needle);
        if !matched {
            continue;
        }
        if row.add {
            if recents >= RECENT_WORKSPACES {
                continue;
            }
            recents += 1;
        }
        out.push(at);
    }
    out
}

impl PickerDelegate for PaletteDelegate {
    type Row = PaletteItem;
    type Pick = PalettePick;

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn update_matches(&mut self, query: &str) {
        self.query = query.to_owned();
        self.matches = match &self.rows {
            PaletteRows::Commands(commands) => command_matches(commands, query),
            PaletteRows::Projects(rows) => project_matches(rows, query),
            PaletteRows::Plain(rows) => (0..rows.len()).collect(),
            PaletteRows::Search { rows, .. } => (0..rows.len()).collect(),
        };
    }

    fn match_id(&self, ix: usize) -> Option<SharedString> {
        self.row(ix).map(|(id, ..)| id)
    }

    fn confirm(&self, ix: usize) -> Option<PalettePick> {
        match self.match_id(ix) {
            Some(id) => {
                crate::baaz_log!("palette select (keyboard): {:?} {id}", self.kind);
                Some(PalettePick::Row(id))
            }
            None => {
                crate::baaz_log!("palette confirm: nothing at {ix} of {} rows", self.match_count());
                // The Add section's lead is a card, not a row, so an empty
                // Projects palette still offers the panel on ↩.
                (self.kind == PaletteKind::Projects).then_some(PalettePick::ChooseFolder)
            }
        }
    }

    fn confirm_id(&self, id: &SharedString) -> Option<PalettePick> {
        // The click carries the row id, so it runs that row directly
        // (finding `performance-7`).
        crate::baaz_log!("palette select (click): {:?} {id}", self.kind);
        Some(PalettePick::Row(id.clone()))
    }

    /// docs/83 §9 (19 Nov 2026): one row-high `t-body` `fg-muted` line. A
    /// query that left nothing says so; `/resume` and `/fork` say what is
    /// missing. The search palette has none: its results land after the
    /// query does, so an empty list there is often one still coming, and its
    /// status line already says "No matches". The Projects palette always
    /// draws its Add card, so it never reaches this.
    fn empty_text(&self) -> Option<SharedString> {
        match self.kind {
            PaletteKind::Commands if !self.query.trim().is_empty() => Some("Nothing matches".into()),
            PaletteKind::Resume => Some("No sessions yet".into()),
            PaletteKind::Fork => Some("No completed turns yet".into()),
            PaletteKind::Commands | PaletteKind::Projects | PaletteKind::Search => None,
        }
    }

    fn render_match(&self, ix: usize, _selected: bool) -> PaletteItem {
        let Some((id, label, detail, badge, icon)) = self.row(ix) else {
            return PaletteItem::new("", PaletteIcon::Glyph(IconName::Slash), "");
        };
        let mut item = PaletteItem::new(id, icon, label).context(detail);
        if let Some(badge) = badge {
            item = item.badge(badge);
        }
        // The match emphasis covers the label: the Commands and Projects
        // queries trimmed, the search query as typed.
        match self.kind {
            PaletteKind::Commands | PaletteKind::Projects => {
                let needle = self.query.trim();
                if needle.is_empty() { item } else { item.matching(needle) }
            }
            PaletteKind::Search => {
                if self.query.trim().is_empty() { item } else { item.matching(&self.query) }
            }
            PaletteKind::Resume | PaletteKind::Fork => item,
        }
    }

    fn render_picker(&self, rows: Vec<PaletteItem>, frame: PickerFrame, _window: &mut Window, cx: &mut Context<PaletteView>) -> AnyElement {
        let (placeholder, sections): (SharedString, Vec<PaletteSection>) = match self.kind {
            PaletteKind::Commands => ("Every command in this build".into(), vec![PaletteSection::new("Commands", rows)]),
            PaletteKind::Resume => ("Resume a session in this workspace".into(), vec![PaletteSection::new("Sessions", rows)]),
            PaletteKind::Fork => ("Pick a completed turn to branch from".into(), vec![PaletteSection::new("Fork from", rows)]),
            PaletteKind::Projects => {
                // The sections only partition the rows. The Add section
                // always shows: its lead is the library's folder card, the
                // first thing under "ADD" above the recent workspaces, so a
                // click chooses through the system panel and a drop adopts
                // every dropped directory. It is not a row — the keyboard
                // walks past it.
                let me = cx.entity().downgrade();
                let choose = move |window: &mut Window, cx: &mut App| {
                    let _ = me.update(cx, |picker, cx| picker.pick(PalettePick::ChooseFolder, window, cx));
                };
                let me = cx.entity().downgrade();
                let drop = move |paths: Vec<PathBuf>, window: &mut Window, cx: &mut App| {
                    let _ = me.update(cx, |picker, cx| picker.pick(PalettePick::Drop(paths), window, cx));
                };
                let lead = folder_drop_card("projects-drop").key("⌘⇧O").on_click(choose).on_drop(drop);
                let (mut adopted, mut adding) = (Vec::new(), Vec::new());
                for (ix, item) in rows.into_iter().enumerate() {
                    if self.adds(ix) {
                        adding.push(item);
                    } else {
                        adopted.push(item);
                    }
                }
                // A section with no rows draws nothing (the library drops
                // it); the Add section keeps its card.
                let sections = vec![PaletteSection::new("Projects", adopted), PaletteSection::new("Add", adding).lead(lead)];
                ("Add or switch project".into(), sections)
            }
            PaletteKind::Search => {
                let (sessions, files): (Vec<_>, Vec<_>) = rows.into_iter().partition(|item| item.id.starts_with("s:"));
                (self.status().cloned().unwrap_or_default(), vec![PaletteSection::new("Sessions", sessions), PaletteSection::new("Files", files)])
            }
        };
        let (select, dismiss) = (frame.select.clone(), frame.dismiss.clone());
        let mut card = command_palette("palette", "", sections, frame.selected)
            .placeholder(placeholder)
            .on_select(move |id, window, cx| select(id, window, cx))
            .on_dismiss(move |window, cx| dismiss(window, cx));
        if let Some(empty) = frame.empty.clone() {
            card = card.empty(empty);
        }
        // A scripted screenshot is a static composition, not an opening:
        // the card's enter presence never settles inside a capture.
        if self.still {
            card = card.at_rest();
        }
        let dismiss = frame.dismiss.clone();
        let layer = dismiss_layer("palette-scrim", "Close the palette", move |window, cx| dismiss(window, cx));
        let mut frame = frame;
        if let Some(query) = frame.query.take() {
            card = card.query_slot(query);
        }
        popover_layer(
            // The open palette owns the wheel: the layer occludes, so the
            // transcript's `wheel_capture` yields everywhere outside the
            // card, and a wheel over the dimmed ground never reaches the
            // transcript behind it. A click on the dimmed ground closes it;
            // the layer dismisses on the release, after a row's own click.
            frame
                .keyed(layer)
                .bg(aui::overlay::scrim())
                // Centred, its top at 18 % of the window (docs/83 §7): a
                // spacer of that share of the layer's height, then the card.
                .child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .child(div().flex_none().w_full().h(gpui::relative(scale::PALETTE_TOP)))
                        .child(card),
                ),
        )
        .into_any_element()
    }
}

// ── the root's side ─────────────────────────────────────────────────────

impl Harness {
    /// The open palette's view.
    pub(crate) fn palette_view(&self, cx: &App) -> Option<Entity<PaletteView>> {
        self.overlays.read(cx).stack.view_as::<PaletteView>(LayerKind::Palette)
    }

    /// Open the palette on one list. Opening the list that is open closes
    /// it. Merely opening never exits Settings (B11b): only a palette
    /// action executed exits — see [`Self::run_palette_row`] — so opening
    /// and cancelling keeps the page exactly where it was.
    pub(crate) fn open_palette(&mut self, kind: PaletteKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).palette().is_some_and(|p| p.kind == kind);
        if already {
            self.overlays.update(cx, |overlays, cx| overlays.close_palette(cx));
            return;
        }
        self.show_palette(kind, cx);
    }

    /// Put the palette up on `kind`, replacing whatever list it showed.
    pub(crate) fn show_palette(&mut self, kind: PaletteKind, cx: &mut Context<Self>) -> Entity<PaletteView> {
        let delegate = PaletteDelegate::new(kind, self.palette_facts(kind, cx), self.still());
        let view = match kind {
            // Commands and Projects type in a field of their own; the search
            // palette's field is the root's, because the search reads it.
            PaletteKind::Commands | PaletteKind::Projects => {
                let placeholder = if kind == PaletteKind::Commands { "Every command in this build" } else { "Add or switch project" };
                cx.new(|cx| Picker::with_own_field(delegate, placeholder, cx))
            }
            PaletteKind::Search => {
                let field = self.search_query.clone();
                cx.new(|cx| Picker::with_field(delegate, field, cx))
            }
            PaletteKind::Resume | PaletteKind::Fork => cx.new(|cx| Picker::new(delegate, cx)),
        };
        let picked = cx.subscribe(&view, move |this: &mut Self, _, event: &Picked<PalettePick>, cx| {
            let pick = event.0.clone();
            this.run_on_main(cx, move |this, window, cx| this.on_palette_pick(kind, pick, window, cx));
        });
        let entry = Entry::new(Layer::Palette(Palette { kind })).keep(picked);
        self.push_overlay_view(entry, &view, cx);
        view
    }

    /// What the palette on `kind` lists now.
    pub(crate) fn palette_facts(&self, kind: PaletteKind, cx: &App) -> PaletteRows {
        match kind {
            PaletteKind::Commands => {
                let provider = self.active_provider(cx);
                let skills = crate::skills_page::skills_offered(cx);
                PaletteRows::Commands(
                    Command::ALL
                        .into_iter()
                        .filter(|command| provider.is_none_or(|id| command.offered_on(id)))
                        .filter(|command| *command != Command::Skills || skills)
                        .collect(),
                )
            }
            PaletteKind::Resume => PaletteRows::Plain(
                self.visible_sessions(cx)
                    .iter()
                    // Newest first, and only as many as the palette can
                    // show: a list taller than the window is a list with a
                    // hidden bottom.
                    .take(crate::app::PALETTE_ROWS)
                    .map(|entry| {
                        let meta: SharedString =
                            if entry.turns > 0 { format!("{} turns", entry.turns).into() } else { "no turns".into() };
                        (entry.id.clone().into(), entry.label.clone().into(), meta)
                    })
                    .collect(),
            ),
            // The active session's completed turns, newest first; the rows
            // come from the view because the window keeps no transcript.
            PaletteKind::Fork => PaletteRows::Plain(
                self.active
                    .as_ref()
                    .map(|view| view.read(cx).rt(cx).fork_turns())
                    .unwrap_or_default()
                    .into_iter()
                    .take(crate::app::PALETTE_ROWS)
                    .map(|(id, label, detail)| (id.into(), label.into(), detail.into()))
                    .collect(),
            ),
            PaletteKind::Search => PaletteRows::Search {
                // Every row wears its project's display name (the folder
                // name for sessions no project holds), so hits from several
                // projects tell themselves apart.
                rows: self
                    .search_rows(cx)
                    .into_iter()
                    .map(|(id, label, detail)| {
                        let session = id
                            .strip_prefix("s:")
                            .or_else(|| id.strip_prefix("f:").and_then(|rest| rest.split_once(':').map(|(session, _)| session)));
                        let badge = session.and_then(|session| self.search_badge(session, cx)).map(SharedString::from);
                        (id, label, detail, badge)
                    })
                    .collect(),
                status: self.search_status(cx),
            },
            PaletteKind::Projects => PaletteRows::Projects(self.project_candidates(cx)),
        }
    }

    /// The palette for this frame, its rows pushed first (the palette draws
    /// right after, in this same frame), or `None` when none is open.
    pub(crate) fn compose_palette(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = self.palette_view(cx)?;
        let kind = view.read(cx).delegate.kind;
        let rows = self.palette_facts(kind, cx);
        view.update(cx, |picker, cx| picker.push_facts(cx, |delegate| delegate.set_rows(rows)));
        Some(view.into_any_element())
    }

    /// What a palette row (or its folder card) asked for.
    fn on_palette_pick(&mut self, kind: PaletteKind, pick: PalettePick, window: &mut Window, cx: &mut Context<Self>) {
        match pick {
            PalettePick::Row(id) => self.run_palette_row(kind, id, window, cx),
            PalettePick::ChooseFolder => self.choose_project_folder(cx),
            PalettePick::Drop(paths) => self.adopt_dropped(paths, cx),
        }
    }

    /// Open the Projects palette: adopted projects to start a session in,
    /// then the folder card and recent Muse workspaces to adopt. Picking a
    /// project starts a session there (and makes it current); adopting only
    /// adopts. `focus_add` starts the selection on the Add section, which
    /// is where the hero's "Recent workspaces" button points.
    pub(crate) fn open_projects(&mut self, focus_add: bool, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.show_palette(PaletteKind::Projects, cx);
        let selected = if focus_add {
            self.project_candidates(cx).iter().position(|row| row.add).unwrap_or(0)
        } else {
            0
        };
        view.update(cx, |picker, cx| picker.select(selected, cx));
        window.focus(&view.focus_handle(cx), cx);
        cx.notify();
    }

    /// The Projects palette's candidates, in the order it lists them.
    /// Section "Projects" holds every adoption in sidebar order (the root
    /// with `~` for home, the visible session count); section "Add" holds
    /// recent Muse workspaces from the index — minus adopted roots and
    /// Baaz's own state, only paths that still exist as directories, newest
    /// first, badged with their session count. The palette's query narrows
    /// both and keeps the first twelve recent ones that match.
    pub(crate) fn project_candidates(&self, cx: &App) -> Vec<ProjectRow> {
        let mut rows = Vec::new();
        let visible = self.visible_sessions(cx);
        for project in self.ordered_projects(cx) {
            let root = project.root.to_string_lossy().into_owned();
            let count = visible.iter().filter(|e| e.project.as_deref() == Some(project.id.as_str())).count();
            // No coloured tiles anywhere: adopted projects wear the folder
            // glyph, like the recent workspaces below.
            rows.push(ProjectRow {
                id: SharedString::from(format!("p:{}", project.id)),
                add: false,
                name: project.name.clone(),
                context: tilde_root(&root),
                path: root,
                count,
            });
        }
        let adopted: std::collections::HashSet<String> = self
            .session_list.read(cx).projects
            .projects
            .iter()
            .map(|p| crate::projects::canonical_path(&p.root).to_string_lossy().into_owned())
            .collect();
        // Baaz's own state directory holds the tier probe's throwaway
        // workspace; it is never a project anyone means to adopt.
        let own_state = [crate::store::support_dir(), crate::store::default_support_dir()];
        for (root, count, _) in crate::index::workspaces(&self.session_list.read(cx).index) {
            if adopted.contains(&crate::projects::canonical_str(&root)) {
                continue;
            }
            if own_state.iter().any(|dir| std::path::Path::new(&root).starts_with(dir)) {
                continue;
            }
            if !std::path::Path::new(&root).is_dir() {
                continue;
            }
            let name = std::path::Path::new(&root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.clone());
            rows.push(ProjectRow {
                id: SharedString::from(format!("a:{root}")),
                add: true,
                name,
                context: root.clone(),
                path: root,
                count,
            });
        }
        rows
    }


    /// The open palette's query field, once its first frame made it.
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn palette_query(&self, cx: &App) -> Option<Entity<gpui_kit::base::input::TextareaState>> {
        self.palette_view(cx).and_then(|view| view.read(cx).field().cloned())
    }

    /// The palette's rows on `kind` (id, label, detail) in the order the
    /// keyboard walks them: the open palette's, narrowed by its query, or
    /// what a palette opened now would list.
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn palette_rows(&self, kind: PaletteKind, cx: &App) -> Vec<(SharedString, SharedString, SharedString)> {
        let open = self.palette_view(cx).filter(|view| view.read(cx).delegate.kind == kind);
        let rows = |delegate: &PaletteDelegate| {
            (0..delegate.match_count()).filter_map(|ix| delegate.row(ix).map(|(id, label, detail, ..)| (id, label, detail))).collect()
        };
        match open {
            Some(view) => rows(&view.read(cx).delegate),
            None => rows(&PaletteDelegate::new(kind, self.palette_facts(kind, cx), true)),
        }
    }

    /// How many rows the palette on `kind` lists (see [`Self::palette_rows`]).
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn palette_rows_len_for(&self, kind: PaletteKind, cx: &App) -> usize {
        self.palette_rows(kind, cx).len()
    }

    /// The open palette's selection.
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn palette_selected(&self, cx: &App) -> Option<usize> {
        self.palette_view(cx).map(|view| view.read(cx).selected())
    }
}

/// A root with `~` for home, for the Projects section's context line.
pub(crate) fn tilde_root(root: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().into_owned();
        if let Some(rest) = root.strip_prefix(home.as_str()) {
            if rest.is_empty() {
                return "~".to_owned();
            }
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    root.to_owned()
}

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, add: bool, name: &str) -> ProjectRow {
        ProjectRow { id: id.into(), add, name: name.into(), path: format!("/w/{name}"), context: format!("/w/{name}"), count: 0 }
    }

    #[test]
    fn the_projects_query_keeps_twelve_recent_workspaces_that_match() {
        let mut rows = vec![project("p:1", false, "alpha")];
        rows.extend((0..20).map(|n| project(&format!("a:{n}"), true, &format!("ws{n}"))));
        assert_eq!(project_matches(&rows, "").len(), 1 + RECENT_WORKSPACES);
        assert_eq!(project_matches(&rows, "ws1").len(), 11, "ws1 and ws10..ws19: the cap counts matches");
        assert_eq!(project_matches(&rows, "alp"), vec![0]);
    }

    #[test]
    fn the_commands_query_matches_slash_and_description() {
        let all: Vec<Command> = Command::ALL.to_vec();
        assert_eq!(command_matches(&all, "").len(), all.len());
        let page = command_matches(&all, " /PAG ");
        assert!(page.iter().any(|at| all[*at] == Command::RightBrowser));
    }
}
