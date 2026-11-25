//! `Overlays`: everything that floats above the window (spec §2.3).
//!
//! One entity, and it holds **state only**: the overlay stack
//! ([`crate::overlay_stack`] — the modal, the palette, the menu and where its
//! selection is, the popovers, the sheets, the inline renames), the toasts
//! waiting to be shown, and the two lists the menus are built from — the
//! workspace's files and the CLI's skills, both walked once at boot and both
//! window-wide rather than per-session. The elements themselves are drawn by
//! whoever anchors them. A dialog, a sheet, a palette and a picker are their
//! own views (plan 4.6: [`crate::dialogs::DialogView`], the sheets, the
//! [`crate::picker::Picker`]s), held by their stack entry and dismissed
//! through it; the root seats them and draws the toast stack; the composer's
//! chip menus, caret popovers and `+` menu are seated by
//! [`crate::session::SessionView`], because the design rule is that a picker
//! is anchored to the chip that opened it and only the composer knows where
//! its chips are; the right pane, the Settings page, the Skills page and the
//! connect screen draw their own entries. The main window's ones go through
//! `aui::overlay::popover_layer`, so the paint order is still the single one
//! the library defines.
//!
//! Keeping the state here rather than in the views is what makes "Escape
//! closes the newest overlay" one pop instead of a negotiation.

use aui::feedback::{ToastAction, ToastData, ToastKind};
use aui::overlay::DialogKind;
use aui_protocol::ReasoningEffort;

use crate::overlay_stack::{Entry, Layer, LayerKind, OverlayStack};

/// One modal on screen. Only one at a time.
#[derive(Clone)]
pub struct Dialog {
    /// Heading.
    pub title: String,
    /// Body paragraph.
    pub detail: String,
    /// Which tint the dialog wears.
    pub kind: DialogKind,
    /// The primary button's label.
    pub primary: &'static str,
    /// What the primary button does.
    pub action: DialogAction,
    /// When the action archives, the session it archives; when it removes a
    /// project, that project's id; when it closes a terminal tab, that
    /// tab's id. The target lives on the dialog so dismissing it — Escape,
    /// the scrim, Cancel — drops the target with it and nothing can confirm
    /// afterwards.
    pub archive_target: Option<String>,
}

/// What a dialog's primary button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogAction {
    /// Close it and carry on.
    Dismiss,
    /// Respawn `muse serve` and resume.
    Reconnect,
    /// Archive the dialog's `archive_target` out of the sidebar.
    Archive,
    /// Remove the dialog's `archive_target` project from the sidebar (its
    /// sessions stay on disk and move to Unfiled).
    RemoveProject,
    /// Quit the app after confirming a still-running terminal command (D52).
    QuitWithWorkRunning,
    /// Close the dialog's `archive_target` terminal tab after confirming
    /// its still-running command (D52).
    CloseTerminalTab,
    /// Start the handoff the dialog previews: the facts ride on the
    /// application (`HandoffState::confirm`), not on this action, so
    /// dismissing any other way confirms nothing.
    HandoffConfirm,
    /// Sign out of the provider in `archive_target` (its wire id): a
    /// real CLI logout, confirmed first. Dismissing any other way signs
    /// out nothing.
    ProviderSignOut,
    /// Delete the browser space in `archive_target` and close the tabs it
    /// holds (the space-chip menu asks first when it has any).
    DeleteSpace,
    /// Clear all browsing history (R4): confirmed, it cannot be undone.
    ClearHistory,
    /// Clear every project's prompt history (Settings › Data, docs/82
    /// 8.8): confirmed, it cannot be undone; sessions stay.
    ClearPromptHistory,
    /// Show the crash report in `archive_target` (its path) in Finder: the
    /// one-time notice after Baaz quit unexpectedly (docs/82 9.5).
    RevealCrash,
    /// Flip a space's own sign-ins (R4); the space id rides the dialog.
    SpaceSignIns,
    /// Move Baaz's own provider sessions into Baaz's homes (B4M): the
    /// one-time prompt's Move. The plan runs through the journaling
    /// executor; dismissing any other way moves nothing.
    MigrateSessions,
    /// Move a forgotten session's transcript, still in Baaz's own home, to
    /// the Trash (D-8). The paths ride the dialog's `archive_target`, one
    /// per line; dismissing any other way keeps them.
    TrashTranscript,
    /// Delete the session in `archive_target` for good (docs/82 8.5): what
    /// Baaz kept for it goes, and its transcript is offered for the Trash
    /// ([`Self::TrashTranscript`]). Only this button deletes; dismissing
    /// any other way keeps everything.
    DeleteSession,
    /// Undo a turn or revert a file (docs/82 6.4): the session and the
    /// ask's number ride `archive_target` (`<session id>\n<n>`), so only
    /// this button runs the action; dismissing any other way changes
    /// nothing.
    UndoChanges,
    /// An agent's `changes_commit {push: true}` (docs/85 13.4, rule 29):
    /// Baaz's own approval for the push. The ask's number rides
    /// `archive_target`; dismissing any other way pushes nothing.
    AgentPush,
}

/// Which popover is open over the composer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    /// The model picker, anchored to the model chip.
    Model,
    /// The reasoning-effort picker, anchored to the effort chip.
    Effort,
    /// The approval-mode picker, anchored to the mode chip.
    Mode,
    /// The provider picker, anchored to the composer's provider chip.
    Provider,
    /// The new-session row's folder picker (docs/82 6.6), anchored to its
    /// folder control.
    Folder,
    /// The `/` command menu, above the composer.
    Command,
    /// The `@` mention picker, above the composer.
    Mention,
    /// The header's ⋯: the session's one menu (the row's rows) plus what only the open session has.
    Overflow,
    /// The Sessions caption's view menu: empty and archived filters.
    ViewOptions,
    /// The footer's account menu: Sign out.
    Account,
    /// A project's menu: the header crumb's, or a group row's `…` tray.
    /// The target project id lives on [`Menu::project`] (`None` is "Other
    /// workspaces"), because the kind itself stays `Copy`.
    Project,
    /// A sidebar session row's `⋯` menu (docs/83 §4): Pin, Rename,
    /// Archive. The session id lives on [`Menu::session`]; the screen draws
    /// and drives it.
    SessionRow,
    /// The right pane's view menu, under the tab row's `＋` (docs/83 §6):
    /// Changes, Files, Page, Terminal, Tasks, and (in Sessions) the views that
    /// can stack below (docs/82 8.1), the ones showing checked.
    RightViews,
    /// The composer's `+` menu: attach, mention, slash commands. A menu like
    /// the chip pickers, so one menu stands at a time with no extra rule.
    Plus,
}

/// An open menu: which one, where the keyboard is, and what has been typed.
pub struct Menu {
    /// Which popover.
    pub kind: MenuKind,
    /// The highlighted row, across all sections in order.
    pub selected: usize,
    /// What has been typed after the `/` or the `@`. The chip pickers do not
    /// filter, so it stays empty for them.
    pub filter: String,
    /// Byte offset in the draft where the `/` or `@` sits, so the caret popovers
    /// know exactly what to replace when a row is picked.
    pub at: usize,
    /// For [`MenuKind::Project`]: the menu's project id, or `None` for the
    /// Unfiled group. Meaningless for every other kind.
    pub project: Option<String>,
    /// For [`MenuKind::Project`]: opened from the header crumb rather than a
    /// group row, so the menu anchors under the crumb instead of the
    /// sidebar's content edge.
    pub project_header: bool,
    /// The menu's submenu is open: the model picker's effort submenu on
    /// the selected row (→ opens it, ← closes it; D4), the project menu's
    /// Colour submenu.
    pub sub_open: bool,
    /// For [`MenuKind::SessionRow`]: whose row menu it is. Meaningless for
    /// every other kind.
    pub session: Option<String>,
    /// For [`MenuKind::SessionRow`]: where a right-click on the row landed,
    /// in window coordinates — the menu stands there. `None`: under the
    /// row's `⋯` (docs/82 8.5).
    pub point: Option<gpui::Point<gpui::Pixels>>,
}

impl Menu {
    /// Move the selection by `delta` rows within `count`, wrapping; whether
    /// it moved.
    pub fn step(&mut self, delta: isize, count: usize) -> bool {
        let before = self.selected;
        if count == 0 {
            self.selected = 0;
        } else {
            let count = count as isize;
            self.selected = (((self.selected as isize + delta) % count + count) % count) as usize;
        }
        self.selected != before
    }

    /// A chip picker with the selection on `selected`.
    pub fn picker(kind: MenuKind, selected: usize) -> Self {
        Self { kind, selected, filter: String::new(), at: 0, project: None, project_header: false, sub_open: false, session: None, point: None }
    }

    /// A caret popover that started at byte `at` in the draft.
    pub fn caret(kind: MenuKind, at: usize) -> Self {
        Self { kind, selected: 0, filter: String::new(), at, project: None, project_header: false, sub_open: false, session: None, point: None }
    }

    /// A project menu for `project` (`None` is Unfiled).
    pub fn project(project: Option<String>, from_header: bool) -> Self {
        Self { kind: MenuKind::Project, selected: 0, filter: String::new(), at: 0, project, project_header: from_header, sub_open: false, session: None, point: None }
    }

    /// A sidebar session row's `⋯` menu for `session`.
    pub fn session_row(session: String) -> Self {
        Self { session: Some(session), ..Self::picker(MenuKind::SessionRow, 0) }
    }

    /// The same row menu, opened by a right-click at `point` (docs/82 8.5).
    pub fn session_row_at(session: String, point: gpui::Point<gpui::Pixels>) -> Self {
        Self { point: Some(point), ..Self::session_row(session) }
    }
}

/// Which list the command palette is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteKind {
    /// ⌘K: every `/` command and every session operation.
    Commands,
    /// `/resume`: the workspace's sessions, under the titles the sidebar shows.
    Resume,
    /// `/fork` with nothing named: the session's completed assistant turns,
    /// newest first, under the user prompt that started each one.
    Fork,
    /// `/search`, the sidebar search icon and Cmd+Shift+F: full-text matches
    /// over past sessions plus the files this workspace's turns created.
    Search,
    /// ⌘⇧O, File › Add Project…, the sidebar row, `/project`: adopted
    /// projects to switch to, then recent Muse workspaces to adopt.
    Projects,
}

/// The open command palette: which list. The palette's view
/// ([`crate::palette::PaletteView`]) keeps the rest — its rows, its query,
/// where the keyboard is.
pub struct Palette {
    /// Which list.
    pub kind: PaletteKind,
}

/// How many roots the `@` index and the skills cache hold: one window over
/// several workspaces, not an unbounded map. Matches the parked-view MRU, so
/// a root with an open view is never the one dropped.
pub(crate) const MAX_CACHED_ROOTS: usize = 8;

/// The prefix of an Open toast action's id; the session id follows.
const OPEN_ACTION: &str = "open:";

/// The session an Open toast action takes the person to, from its id
/// ([`Overlays::toast_open`]); `None` for any other action (Undo).
pub(crate) fn toast_open_target(action: &str) -> Option<&str> {
    action.strip_prefix(OPEN_ACTION).filter(|session| !session.is_empty())
}

/// How long a toast stays when nobody dismisses it: the Undo window, so an
/// Undo toast and a plain one leave on the same beat.
pub(crate) const TOAST_LIFE: std::time::Duration = std::time::Duration::from_secs(8);

/// The floating state of the window.
#[derive(Default)]
pub struct Overlays {
    /// Every open overlay, oldest first: the dialog, the palette, the menus,
    /// the popovers, the sheets ([`crate::overlay_stack`]).
    pub(crate) stack: OverlayStack,
    /// Toasts, oldest first.
    pub toasts: Vec<ToastData>,
    /// The `@` picker's candidates per root: the root's files, relative to
    /// it, lowercased once at walk time (see [`crate::files::FileEntry`]).
    files_by_root: std::collections::HashMap<String, crate::files::WalkResult>,
    /// The `/` menu's **Skills** section per root, from `muse skills list
    /// --json` with that root as the working directory.
    skills_by_root: std::collections::HashMap<String, Vec<crate::skills::Skill>>,
    /// Roots in walk order, oldest first: what the cap above evicts from.
    roots_order: Vec<String>,
    /// Monotonic id source, so two identical toasts are still two toasts.
    next_toast: u64,
    /// Where the shell's own menus seat ([`Anchor`]), in window
    /// coordinates, recorded once per frame by `on_children_prepainted`
    /// wrappers on their triggers — what `aui::overlay::anchored_menu`
    /// seats them at (menus anchor to their triggers, never constants).
    anchors: [Option<gpui::Bounds<gpui::Pixels>>; 2],
    /// The transcript's find bar (⌘F, docs/82 7.5): one per window, naming
    /// the session it searches ([`crate::session::find`]). `None` while
    /// closed — the transcript's frame then reads no marks at all.
    pub(crate) transcript_find: Option<gpui::Entity<crate::session::find::TranscriptFind>>,
}

/// A shell menu's anchor ([`Overlays::note_anchor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// The header overflow `…` button: the overflow menu's trigger.
    Overflow,
    /// The open project menu's own rect: the Colour submenu's anchor is
    /// computed off its right edge at the Colour row's height.
    ProjectMenu,
}

impl Overlays {
    // Every mutation below is the owner's: it changes the stack or the
    // toasts and notifies this entity once, when something changed. The root
    // reads the stack in every frame (`modal_overlay_wanted`), so that notify
    // redraws the root and runs its settle (which observes this entity);
    // callers never notify after these (plan 4.9b).

    /// The modal, if any.
    pub fn dialog(&self) -> Option<&Dialog> {
        self.stack.dialog()
    }

    /// Open `entry` on top, replacing an open entry of its kind
    /// ([`OverlayStack::push`]).
    pub(crate) fn push(&mut self, entry: Entry, cx: &mut gpui::Context<Self>) {
        self.stack.push(entry);
        cx.notify();
    }

    /// Close the entry of `kind`, if open ([`OverlayStack::close`]).
    pub(crate) fn close(&mut self, kind: LayerKind, cx: &mut gpui::Context<Self>) -> Option<Entry> {
        let closed = self.stack.close(kind);
        if closed.is_some() {
            cx.notify();
        }
        closed
    }

    /// Close the top entry ([`OverlayStack::pop`]).
    pub(crate) fn pop(&mut self, cx: &mut gpui::Context<Self>) -> Option<Entry> {
        let popped = self.stack.pop();
        if popped.is_some() {
            cx.notify();
        }
        popped
    }

    /// Close every entry of the `kinds`, as a mode switch does.
    pub(crate) fn close_all(&mut self, kinds: &[LayerKind], cx: &mut gpui::Context<Self>) {
        if kinds.iter().any(|kind| self.stack.is_open(*kind)) {
            self.stack.close_all(kinds);
            cx.notify();
        }
    }

    /// Open `layer` (`open`) or close its kind: a flag-shaped toggle.
    pub(crate) fn set(&mut self, layer: Layer, open: bool, cx: &mut gpui::Context<Self>) {
        if open || self.stack.is_open(layer.kind()) {
            self.stack.set(layer, open);
            cx.notify();
        }
    }

    /// Dismiss the modal.
    pub fn close_dialog(&mut self, cx: &mut gpui::Context<Self>) {
        self.close(LayerKind::Dialog, cx);
    }

    /// The open command palette, if any.
    pub fn palette(&self) -> Option<&Palette> {
        self.stack.palette()
    }

    /// Close the palette.
    pub fn close_palette(&mut self, cx: &mut gpui::Context<Self>) {
        self.close(LayerKind::Palette, cx);
    }

    /// The open popover, if any.
    pub fn menu(&self) -> Option<&Menu> {
        self.stack.menu()
    }

    /// Change the open popover of `kind` (its submenu flag, its selection),
    /// if that is the open one: `edit` returns whether it changed anything.
    pub(crate) fn edit_menu(&mut self, kind: Option<MenuKind>, edit: impl FnOnce(&mut Menu) -> bool, cx: &mut gpui::Context<Self>) {
        let Some(menu) = self.stack.menu_mut().filter(|menu| kind.is_none_or(|kind| menu.kind == kind)) else {
            return;
        };
        if edit(menu) {
            cx.notify();
        }
    }

    /// Change the open import sheet's content: `change` answers `None` when
    /// it changed nothing (and nothing is redrawn).
    pub(crate) fn edit_import_sheet<R>(
        &mut self,
        change: impl FnOnce(&mut crate::import_ui::ImportSheet) -> Option<R>,
        cx: &mut gpui::Context<Self>,
    ) -> Option<R> {
        let changed = self.stack.import_sheet_mut().and_then(change);
        if changed.is_some() {
            cx.notify();
        }
        changed
    }

    /// Change the open Chrome import sheet's content: `change` answers
    /// `None` when it changed nothing (a stale count, a run already going).
    pub(crate) fn edit_chrome_import<R>(
        &mut self,
        change: impl FnOnce(&mut crate::chrome_import_ui::ChromeImportSheet) -> Option<R>,
        cx: &mut gpui::Context<Self>,
    ) -> Option<R> {
        let changed = self.stack.chrome_import_mut().and_then(change);
        if changed.is_some() {
            cx.notify();
        }
        changed
    }

    /// Close the open popover. The root's settle observes the stack and
    /// hands the keyboard back where the menu found it.
    pub fn close_menu(&mut self, cx: &mut gpui::Context<Self>) {
        self.close(LayerKind::Menu, cx);
    }

    /// Open a menu, replacing whatever menu was open.
    pub fn open(&mut self, menu: Menu, cx: &mut gpui::Context<Self>) {
        self.push(Entry::new(Layer::Menu(menu)), cx);
    }

    /// Open one of the composer's own menus (a caret menu or a chip picker),
    /// replacing whatever menu was open: closing it hands the keyboard to
    /// the composer (`composer`) unless another field took it.
    pub(crate) fn open_composer_menu(&mut self, menu: Menu, composer: gpui::FocusHandle, cx: &mut gpui::Context<Self>) {
        self.push(Entry::new(Layer::Menu(menu)).restore_to(composer), cx);
    }

    /// Whether `kind` is the open menu.
    pub fn is_open(&self, kind: MenuKind) -> bool {
        self.menu().is_some_and(|m| m.kind == kind)
    }

    /// Move the selection by `delta` rows within `count`, wrapping.
    pub fn move_selection(&mut self, delta: isize, count: usize, cx: &mut gpui::Context<Self>) {
        self.edit_menu(None, |menu| menu.step(delta, count), cx);
    }

    /// Show a note. Toasts are informational here — a compaction that did
    /// nothing, a command this build does not have — so they carry no action.
    /// Returns the toast's id, so callers with something to retract (a
    /// provider recheck's settled note) can dismiss it later.
    pub fn toast(&mut self, title: impl Into<String>, body: impl Into<String>, cx: &mut gpui::Context<Self>) -> String {
        self.push_toast(|id| ToastData::new(id, title.into(), body.into()).kind(ToastKind::Neutral), cx)
    }

    /// Stack the toast `make` builds under the next id, and return the id.
    fn push_toast(&mut self, make: impl FnOnce(String) -> ToastData, cx: &mut gpui::Context<Self>) -> String {
        self.next_toast += 1;
        let id = format!("toast-{}", self.next_toast);
        self.toasts.push(make(id.clone()));
        cx.notify();
        // A note nobody dismisses used to stay for ever ("Data exported" was
        // still up 20 minutes later, and notes queue behind it). Every toast
        // leaves after [`TOAST_LIFE`]; a capture keeps its toast, because a
        // timer would race the screenshot.
        if !crate::runtime::captures() {
            let gone = id.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TOAST_LIFE).await;
                let _ = this.update(cx, |this, cx| this.dismiss_toast(&gone, cx));
            })
            .detach();
        }
        id
    }

    /// Show a note carrying one action, and return the toast's id.
    ///
    /// Undo is for what the window took away without asking (a hidden
    /// session), and for what it put back after asking (an archive, an
    /// undone turn, a reverted file); Open is [`Self::toast_open`]'s.
    pub fn toast_with_action(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        action: impl Into<String>,
        cx: &mut gpui::Context<Self>,
    ) -> String {
        let label = action.into();
        self.push_toast(
            |id| {
                ToastData::new(id, title.into(), body.into())
                    .kind(ToastKind::Neutral)
                    .action(ToastAction::new(label.to_lowercase(), label).primary())
            },
            cx,
        )
    }

    /// Show where something went (docs/82 8.2: a screenshot, notes, a
    /// hand-off) with Open, which takes the person to `session`. The
    /// action's id carries the session ([`toast_open_target`]).
    pub fn toast_open(&mut self, title: impl Into<String>, body: impl Into<String>, session: &str, cx: &mut gpui::Context<Self>) -> String {
        let action = format!("{OPEN_ACTION}{session}");
        self.push_toast(
            |id| ToastData::new(id, title.into(), body.into()).kind(ToastKind::Neutral).action(ToastAction::new(action, "Open").primary()),
            cx,
        )
    }

    /// Drop the toast with this id.
    pub fn dismiss_toast(&mut self, id: &str, cx: &mut gpui::Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// Record `anchor`'s window bounds, redrawing only when they arrive for
    /// the first time (nothing → something): a menu opened before the first
    /// prepaint appears on the next frame instead of flashing at a wrong
    /// seat, and steady bounds never schedule work of their own.
    pub(crate) fn note_anchor(&mut self, anchor: Anchor, bounds: gpui::Bounds<gpui::Pixels>, cx: &mut gpui::Context<Self>) {
        let slot = &mut self.anchors[anchor as usize];
        let had = slot.is_some();
        *slot = Some(bounds);
        if !had {
            cx.notify();
        }
    }

    /// `anchor`'s last recorded bounds.
    pub(crate) fn anchor(&self, anchor: Anchor) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.anchors[anchor as usize]
    }

    /// Whether `root`'s files and skills are already cached.
    pub fn has_root(&self, root: &str) -> bool {
        self.files_by_root.contains_key(root)
    }

    /// The `@` picker's candidates for `root`: walked files, relative to it.
    /// An unwalked root offers nothing, which is the caller's cue to walk it.
    pub fn files_for(&self, root: &str) -> &[crate::files::FileEntry] {
        self.files_by_root.get(root).map(|walk| walk.entries.as_slice()).unwrap_or(&[])
    }

    /// The `/` menu's **Skills** section for `root`.
    pub fn skills_for(&self, root: &str) -> &[crate::skills::Skill] {
        self.skills_by_root.get(root).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Replace one root's skills without touching its files: the Skills page
    /// pushes each landed catalog here so the `/` menu follows the page
    /// after any mutation (D64).
    pub fn set_skills(&mut self, root: String, skills: Vec<crate::skills::Skill>, cx: &mut gpui::Context<Self>) {
        self.cache_root(root, None, skills, cx);
    }

    /// Store a walked root's files and skills, evicting the least recently
    /// walked past [`MAX_CACHED_ROOTS`]. A re-walk counts as use: the root
    /// moves to the back of the queue.
    pub fn insert_root(
        &mut self,
        root: String,
        files: crate::files::WalkResult,
        skills: Vec<crate::skills::Skill>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.cache_root(root, Some(files), skills, cx);
    }

    /// Cache `root`'s skills, and its files when walked, as the most
    /// recently used root, evicting the oldest past [`MAX_CACHED_ROOTS`];
    /// the menus that list them redraw.
    fn cache_root(
        &mut self,
        root: String,
        files: Option<crate::files::WalkResult>,
        skills: Vec<crate::skills::Skill>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.roots_order.retain(|cached| cached != &root);
        self.roots_order.push(root.clone());
        while self.roots_order.len() > MAX_CACHED_ROOTS {
            let oldest = self.roots_order.remove(0);
            self.files_by_root.remove(&oldest);
            self.skills_by_root.remove(&oldest);
        }
        if let Some(files) = files {
            self.files_by_root.insert(root.clone(), files);
        }
        self.skills_by_root.insert(root, skills);
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// The client-side slash commands (spec §3.10)
// ---------------------------------------------------------------------------

/// One entry of the `/` menu's **Commands** section.
///
/// Every one of these is client-side: MSP has no command plane for them, and
/// the TUI's own `/` palette is a TUI feature. Three of them are named by the
/// spec but belong to later phases; they stay in the list — a command that
/// vanished would read as a command that does not exist — and say so when
/// picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open the model picker.
    Model,
    /// Open the effort picker.
    Effort,
    /// Open the approval-mode picker.
    Mode,
    /// Toggle plan mode.
    Plan,
    /// `session/compact`.
    Compact,
    /// Fork the session.
    Fork,
    /// Rename the session (its row in `baaz.db`, not the wire).
    Name,
    /// Resume another session — the command palette over `session/list`.
    Resume,
    /// Search past sessions and created files.
    Search,
    /// Archive this session (`/archive`; `/hide` still parses onto it —
    /// Hide folded into Archive, docs/82 8.9).
    Archive,
    /// Show sessions with no turns in the sidebar again.
    Empty,
    /// The session status dialog.
    Status,
    /// The session usage dialog (the same dialog as `/status`).
    Usage,
    /// Start a new session in this workspace.
    Clear,
    /// Hand this session's turns to a fresh session on another provider.
    Handoff,
    /// Open the Projects palette: adopt a folder or switch project.
    Project,
    /// `account/logout`.
    Logout,
    /// Show this menu, unfiltered.
    Help,
    /// Open the Skills page.
    Skills,
    /// Show the session's Page in the right pane (`/page`; `/browser`
    /// still parses onto it).
    RightBrowser,
    /// Show session edits and working-tree changes in the right pane.
    RightGit,
    /// Show the file tree in the right pane.
    RightFiles,
    /// Toggle the terminal dock.
    Terminal,
    /// Open a new terminal tab.
    NewTerminalCmd,
    /// Open the next session that needs the person (task 3.4, ⌘⇧J).
    NextWaiting,
}

impl Command {
    /// Whether running this command would touch the right pane or the
    /// terminal dock: the four pane kinds plus the two dock commands.
    /// While the Settings page stands open these are ignored with no state
    /// change (B11b) instead of exiting Settings and acting.
    pub(crate) fn is_pane_or_terminal(&self) -> bool {
        matches!(
            self,
            Command::RightBrowser
                | Command::RightGit
                | Command::RightFiles
                | Command::Terminal
                | Command::NewTerminalCmd
        )
    }
}

impl Command {
    /// Every command, in the order the menu lists them.
    ///
    /// The five window-level commands sit together just before `Help`: the
    /// three right-pane commands as one group, then the two terminal commands
    /// as another, with `Help` still closing the list. Older spellings
    /// (`/diff`, `/handoff`) list no row: [`Command::parse`] maps them onto
    /// the command that replaced them ([`Command::ALIASES`]).
    pub const ALL: [Command; 25] = [
        Command::Model,
        Command::Effort,
        Command::Mode,
        Command::Plan,
        Command::Compact,
        Command::Status,
        Command::Usage,
        Command::Clear,
        Command::Project,
        Command::Handoff,
        Command::Fork,
        Command::Name,
        Command::Resume,
        Command::Search,
        Command::Archive,
        Command::Empty,
        Command::Logout,
        Command::RightBrowser,
        Command::RightGit,
        Command::RightFiles,
        Command::Terminal,
        Command::NewTerminalCmd,
        Command::Skills,
        Command::NextWaiting,
        Command::Help,
    ];

    /// The command as typed, leading slash included.
    /// The provider capability the command's work needs, when it needs one
    /// that not every provider has: the menus offer the command only where
    /// [`crate::providers::offers`] says so (docs/82 0.7).
    pub fn needs(&self) -> Option<provider::Capability> {
        match self {
            Command::Compact => Some(provider::Capability::CompactSession),
            Command::Fork => Some(provider::Capability::ForkSession),
            _ => None,
        }
    }

    /// Whether the command is offered on `id`'s sessions: a capability it
    /// needs, `/plan` only where the provider has a plan mode and `/logout`
    /// only where the account lives on the connection (docs/82 5.5, rule
    /// 13). `/skills` is not the session provider's: it is offered on every
    /// provider while the Skills page works at all
    /// ([`crate::skills_page::skills_offered`], which the menus apply).
    pub fn offered_on(&self, id: crate::providers::ProviderId) -> bool {
        if *self == Command::Plan && id.descriptor().plan == provider::PlanMode::None {
            return false;
        }
        // `/logout` signs out the account on the shared connection: only
        // that provider's sessions have one to sign out of here (the CLIs
        // sign out from Settings › Providers).
        if *self == Command::Logout && !id.has(provider::ProviderTrait::AccountOnConnection) {
            return false;
        }
        self.needs().is_none_or(|capability| crate::providers::offers(id, capability))
    }

    pub fn slash(&self) -> &'static str {
        match self {
            Command::Model => "/model",
            Command::Effort => "/effort",
            Command::Mode => "/mode",
            Command::Plan => "/plan",
            Command::Compact => "/compact",
            Command::Fork => "/fork",
            Command::Name => "/name",
            Command::Resume => "/resume",
            Command::Search => "/search",
            Command::Archive => "/archive",
            Command::Empty => "/empty",
            Command::Status => "/status",
            Command::Usage => "/usage",
            Command::Clear => "/clear",
            Command::Handoff => "/continue",
            Command::Project => "/project",
            Command::Logout => "/logout",
            Command::Help => "/help",
            Command::RightBrowser => "/page",
            Command::RightGit => "/changes",
            Command::RightFiles => "/files",
            Command::Terminal => "/terminal",
            Command::NewTerminalCmd => "/new-terminal",
            Command::Skills => "/skills",
            Command::NextWaiting => "/next-waiting",
        }
    }

    /// What the row says it does.
    pub fn description(&self) -> &'static str {
        match self {
            Command::Model => "Choose the model",
            Command::Effort => "Set the reasoning effort",
            Command::Mode => "Set the approval mode",
            Command::Plan => "Plan first, then approve",
            Command::Compact => "Summarize the conversation to free up context",
            Command::Fork => "Branch this session from an earlier turn",
            Command::Name => "Show or rename this session",
            Command::Resume => "Resume an earlier session",
            Command::Search => "Search sessions and created files",
            Command::Archive => "Archive this session",
            Command::Empty => "Show sessions with no turns in the sidebar",
            Command::Status => "Show current session status",
            Command::Usage => "Show session usage",
            Command::Clear => "Start a new session in this workspace",
            Command::Handoff => "Continue in another provider…",
            Command::Project => "Add or switch project",
            Command::Logout => "Log out and forget the saved login",
            Command::Help => "Show every command",
            Command::RightBrowser => "Show the Page in the right pane",
            Command::RightGit => "Show session edits and working-tree changes in the right pane",
            Command::RightFiles => "Show the file tree in the right pane",
            Command::Terminal => "Toggle the terminal dock",
            Command::NewTerminalCmd => "Open a new terminal tab",
            Command::Skills => "Open the Skills page",
            Command::NextWaiting => "Jump to the next session that needs you",
        }
    }

    /// Parse a typed slash command. An older spelling parses onto the
    /// command that replaced it rather than failing ([`Command::ALIASES`]).
    pub fn parse(text: &str) -> Option<Command> {
        if let Some((_, command)) = Command::ALIASES.iter().find(|(alias, _)| *alias == text) {
            return Some(*command);
        }
        Command::ALL.into_iter().find(|c| c.slash() == text)
    }

    /// Older spellings that still parse, each onto the command that
    /// replaced it; none lists a row of its own. `/diff` is `/changes`;
    /// `/handoff` is `/continue` (docs/07 §1a: one verb, Continue in…);
    /// `/browser` is `/page` (one name for the pane's web view); `/hide`
    /// is `/archive` (Hide folded into Archive).
    pub const ALIASES: [(&'static str, Command); 4] = [
        ("/diff", Command::RightGit),
        ("/handoff", Command::Handoff),
        ("/browser", Command::RightBrowser),
        ("/hide", Command::Archive),
    ];

    /// Parse a whole typed line into a command and whatever followed it.
    ///
    /// `"/name Fix the parser"` is `(Name, "Fix the parser")`, `"/status"` is
    /// `(Status, "")`, and anything that is not a command is `None` — which is
    /// what keeps a prompt beginning with a slash a prompt.
    pub fn parse_line(text: &str) -> Option<(Command, &str)> {
        let line = text.trim_end();
        if !line.starts_with('/') || line.lines().count() > 1 {
            return None;
        }
        let (head, rest) = match line.split_once(' ') {
            Some((head, rest)) => (head, rest.trim_start()),
            None => (line, ""),
        };
        Command::parse(head).map(|command| (command, rest))
    }
}

/// The effort picker's answer for one session: either the levels the current
/// provider and model offer, or the typed reason there is no control — the
/// same shape the model picker uses for an empty catalog. Never an empty
/// menu and never a dead click: the reason row acts (restate, close) like
/// every other row.
pub enum EffortOptions {
    /// The levels to list, `Default` first. Details are the provider's own
    /// text where it sends any, else [`effort_detail`].
    Available(Vec<EffortOption>),
    /// Like [`EffortOptions::Available`], plus a muted note the menu shows
    /// with the list — e.g. an off-catalog Codex model whose levels are the
    /// catalog's union, which the server validates per turn.
    AvailableWithNote { options: Vec<EffortOption>, note: String },
    /// No reasoning control here, and why.
    Unavailable(String),
}

impl EffortOptions {
    /// The listed levels, when the menu lists any.
    pub fn options(&self) -> Option<&[EffortOption]> {
        match self {
            EffortOptions::Available(options) | EffortOptions::AvailableWithNote { options, .. } => {
                Some(options)
            }
            EffortOptions::Unavailable(_) => None,
        }
    }

    /// The muted note riding the list, when one rides it.
    pub fn note(&self) -> Option<&str> {
        match self {
            EffortOptions::AvailableWithNote { note, .. } => Some(note),
            EffortOptions::Available(_) | EffortOptions::Unavailable(_) => None,
        }
    }
}

/// One row of an available effort menu: the level and its detail line.
pub struct EffortOption {
    /// The level; `None` is "Default", which omits the field.
    pub effort: Option<ReasoningEffort>,
    /// The detail line under the row: the provider's description, or the
    /// static [`effort_detail`] fallback when it sends none.
    pub detail: String,
}

/// The reason row's id in the effort menu, mirroring the model picker's
/// unavailable row: clicking it restates the reason and closes.
pub const EFFORT_UNAVAILABLE_ROW: &str = "__effort_unavailable";

/// The row id an effort level lists under: `"default"`, else the variant
/// name. The menu writes these ids and reads them back on pick, so both
/// sides go through this one function.
pub fn effort_row_id(effort: Option<ReasoningEffort>) -> String {
    effort.map(|e| format!("{e:?}")).unwrap_or_else(|| "default".to_owned())
}

/// The full closed enum as a menu — what the muse lane offers when the
/// host names no tiers for the model (pre-1.4.9, or `"unknown"`): MSP
/// accepts every tier including the `max` tier muse 1.1.1 added between
/// `xhigh` and `ultra`. Other lanes never read this; they derive
/// their list from their own catalog (Codex, per model) or offer no
/// control at all (Claude Code, with its reason).
pub fn muse_efforts() -> Vec<EffortOption> {
    [
        None,
        Some(ReasoningEffort::None),
        Some(ReasoningEffort::Minimal),
        Some(ReasoningEffort::Low),
        Some(ReasoningEffort::Medium),
        Some(ReasoningEffort::High),
        Some(ReasoningEffort::Xhigh),
        Some(ReasoningEffort::Max),
        Some(ReasoningEffort::Ultra),
    ]
    .into_iter()
    .map(|effort| EffortOption { effort, detail: effort_detail(effort).to_owned() })
    .collect()
}

/// The label for an effort slot; `None` is "Default", which omits the field.
pub fn effort_label(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "Default",
        Some(effort) => effort.label(),
    }
}

/// The one-line description under an effort row.
pub fn effort_detail(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "Leave it to the provider; the field is omitted.",
        Some(ReasoningEffort::None) => "No reasoning at all.",
        Some(ReasoningEffort::Minimal) => "The smallest budget the provider offers.",
        Some(ReasoningEffort::Low) => "A short budget.",
        Some(ReasoningEffort::Medium) => "The middle budget, and the usual default.",
        Some(ReasoningEffort::High) => "A long budget.",
        Some(ReasoningEffort::Xhigh) => "Longer than high.",
        Some(ReasoningEffort::Max) => "Longer than extra high, below ultra.",
        Some(ReasoningEffort::Ultra) => "The largest budget MSP accepts.",
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_closes_the_newest_overlay_first() {
        let mut overlays = Overlays::default();
        overlays.stack.push(Entry::new(Layer::Dialog(Dialog {
            title: "x".into(),
            detail: "y".into(),
            kind: DialogKind::Error,
            primary: "Dismiss",
            action: DialogAction::Dismiss,
            archive_target: None,
        })));
        overlays.stack.push(Entry::new(Layer::Menu(Menu::picker(MenuKind::Model, 0))));
        assert!(overlays.stack.pop().is_some());
        assert!(overlays.menu().is_none() && overlays.dialog().is_some());
        assert!(overlays.stack.pop().is_some());
        assert!(overlays.stack.pop().is_none());
    }

    #[test]
    fn the_selection_wraps_both_ways() {
        let mut menu = Menu::picker(MenuKind::Effort, 0);
        assert!(menu.step(-1, 3));
        assert_eq!(menu.selected, 2);
        assert!(menu.step(1, 3));
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn an_empty_menu_keeps_the_selection_at_zero() {
        let mut menu = Menu::caret(MenuKind::Command, 0);
        assert!(!menu.step(1, 0));
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn every_command_parses_back_from_its_slash() {
        assert_eq!(Command::ALL.len(), 25, "the palette lists every command");
        for command in Command::ALL {
            assert_eq!(Command::parse(command.slash()), Some(command));
            assert_eq!(Command::parse_line(command.slash()), Some((command, "")));
        }
        // B8: `/diff` is an alias of `/changes`, not its own row.
        assert_eq!(Command::parse("/diff"), Some(Command::RightGit));
        assert_eq!(Command::parse_line("/diff"), Some((Command::RightGit, "")));
        assert_eq!(Command::parse("/changes"), Some(Command::RightGit));
        assert_eq!(Command::parse("/handoff"), Some(Command::Handoff), "the older spelling of /continue");
        assert_eq!(Command::parse("/browser"), Some(Command::RightBrowser), "the older spelling of /page");
        assert_eq!(Command::parse("/hide"), Some(Command::Archive), "Hide folded into Archive");
        for (alias, _) in Command::ALIASES {
            assert!(Command::ALL.iter().all(|command| command.slash() != alias), "{alias} lists no palette row of its own");
        }
        assert_eq!(Command::parse("/nope"), None);
    }

    fn walk_with(paths: &[&str]) -> crate::files::WalkResult {
        crate::files::WalkResult {
            entries: paths
                .iter()
                .map(|path| crate::files::FileEntry { path: path.to_string(), lower: path.to_lowercase() })
                .collect(),
            truncated: false,
        }
    }

    #[gpui::test]
    fn the_per_root_cache_serves_each_root_its_own_files(cx: &mut gpui::TestAppContext) {
        let entity = gpui::AppContext::new(cx, |_| Overlays::default());
        entity.update(cx, |overlays, cx| {
        assert!(overlays.files_for("/work/a").is_empty());
        overlays.insert_root("/work/a".into(), walk_with(&["src/main.rs"]), Vec::new(), cx);
        overlays.insert_root("/work/b".into(), walk_with(&["docs/notes.md"]), Vec::new(), cx);
        assert_eq!(overlays.files_for("/work/a").len(), 1);
        assert_eq!(overlays.files_for("/work/a")[0].path, "src/main.rs");
        assert_eq!(overlays.files_for("/work/b")[0].path, "docs/notes.md");
        assert!(overlays.has_root("/work/a"));
        assert!(!overlays.has_root("/work/c"));
        });
    }

    #[gpui::test]
    fn the_cache_holds_eight_roots_and_re_walk_counts_as_use(cx: &mut gpui::TestAppContext) {
        let entity = gpui::AppContext::new(cx, |_| Overlays::default());
        entity.update(cx, |overlays, cx| {
        for i in 0..MAX_CACHED_ROOTS {
            overlays.insert_root(format!("/work/{i}"), walk_with(&["f"]), Vec::new(), cx);
        }
        assert!(overlays.has_root("/work/0"));
        // Re-walking `/work/0` moves it to the back; the next insert drops
        // `/work/1` instead.
        overlays.insert_root("/work/0".into(), walk_with(&["f"]), Vec::new(), cx);
        overlays.insert_root("/work/new".into(), walk_with(&["f"]), Vec::new(), cx);
        assert!(overlays.has_root("/work/0"));
        assert!(overlays.has_root("/work/new"));
        assert!(!overlays.has_root("/work/1"));
        });
    }
}
