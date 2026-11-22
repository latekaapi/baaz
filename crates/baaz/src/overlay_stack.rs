//! One overlay stack (plan 4.6, audit A R5 / F18).
//!
//! Everything that opens over the window and closes again — a dialog, a
//! palette, a menu, a popover, a sheet, an inline rename — is one typed
//! [`Entry`] on one stack, owned by the [`crate::overlays::Overlays`] entity.
//! Before the stack, three functions each had to know every overlay: Escape
//! (a 14-step ladder in `Harness::cancel`), what covers the native page
//! (`Harness::page_covers`) and focus restore (an edge detector in
//! `Harness::render`). Now each answers from the stack:
//!
//! * **Escape** pops the top entry ([`OverlayStack::pop`]);
//! * **what covers the page** is [`OverlayStack::covers_page`]: every entry
//!   says whether it covers the page and where it draws ([`Host`]) — what an
//!   overlay window draws sits above the live page and covers nothing;
//! * **focus** goes back where it was when the entry opened: the entry
//!   captures the focus of the frame it opened in, and closing it queues that
//!   handle for the root's settle — the end of the effect cycle that closed
//!   it, or the end of the frame that unmounted what held the keyboard
//!   ([`OverlayStack::take_restore`]).
//!
//! An entry carries its content where the content is small (the menu's
//! selection, the rename's target, the sheet's stage), so "is it open" and
//! "what does it show" are one value and cannot disagree. A dialog, a sheet,
//! a palette or a picker is its own view (plan 4.6b): the entry holds that
//! view ([`Entry::view`]), the hosts draw it from here, and the view keeps
//! the content of its own widgets (a query field, the selection, a folder
//! list standing open) and redraws itself. One entry per [`LayerKind`]:
//! opening a kind that is open replaces it in place at the top.
//!
//! A view closes itself one way: it emits [`gpui::DismissEvent`] (its
//! Escape, its ✕, a click away, its panel losing the keyboard), and the root
//! pops the stack newest-first (`Harness::dismiss_overlay_view`) — a menu
//! opened over a sheet closes before the sheet does. What closes an entry for
//! a reason of its own (a row picked, a mode switch) closes it by kind
//! ([`OverlayStack::close`]); the same restore applies. A pop (Escape, a
//! dismissal) additionally runs the entry's own `on_close`, for the entries
//! whose closing does more than drop the entry (a sheet that cancels its
//! flow, a bookmark that keeps what was typed, a rename that refocuses the
//! composer).

use std::rc::Rc;

use gpui::{AnyView, Context, EntityId, FocusHandle, Subscription};

use crate::app::Harness;

use crate::overlays::{Dialog, Menu, MenuKind, Palette};
use crate::layout::LayoutGlobal as _;

/// Where an entry draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    /// In the main window, under the native page: it covers the page when it
    /// says so.
    Main,
    /// In an overlay window ([`crate::overlay`]), above the live page: it
    /// covers nothing.
    Panel,
    /// Where the mode puts a modal (the dialog, the palette, the attach
    /// picker): the main window in Sessions, the overlay window in Browse —
    /// over the live page, so a page loading under an open palette never
    /// moves it.
    Modal,
}

impl Host {
    /// Where it draws in the current mode (`browse`).
    pub(crate) fn resolve(self, browse: bool) -> Host {
        match self {
            Host::Modal if browse => Host::Panel,
            Host::Modal => Host::Main,
            host => host,
        }
    }
}

/// What Escape (or the entry's own view dismissing itself) runs beyond
/// dropping the entry: it gets the entry's content back (a bookmark popover
/// keeps what was typed). The pop runs on the root (`Harness::pop_overlay`,
/// `Harness::dismiss_overlay_view`), which hands itself in: a hook never
/// updates the root re-entrantly.
pub(crate) type OnClose = Rc<dyn Fn(Layer, &mut Harness, &mut Context<Harness>)>;

/// One kind of overlay, with its content.
pub(crate) enum Layer {
    /// The modal dialog.
    Dialog(Dialog),
    /// The command palette (⌘K and its lists): which list; the view
    /// ([`crate::palette::PaletteView`]) holds the rest.
    Palette(Palette),
    /// A menu on the composer, header, sidebar or pane (the [`MenuKind`]s).
    Menu(Menu),
    /// Browse's right-click menu, at the pointer, in the overlay window.
    BrowseMenu(aui::screens::sidebar::BrowseMenu),
    /// The site-information panel under the address line (overlay window).
    SiteInfo,
    /// The bookmark popover under the address line (overlay window).
    Bookmark(gpui::Entity<crate::overlay::BookmarkView>),
    /// Browse's downloads popover.
    Downloads,
    /// The right pane's Files row menu.
    FilesMenu(crate::right_files::FilesMenu),
    /// The right pane's Page row Recent pages menu.
    PageMenu,
    /// The Settings page's assistant picker.
    AssistantMenu,
    /// Settings › General's "Open files in" picker (docs/82 7.5).
    EditorMenu,
    /// The connect screen's footer menu ("New sessions start in").
    ConnectMenu,
    /// The Settings page's "New sessions start in" menu.
    DefaultProviderMenu,
    /// The agent card's hand-off picker.
    HandoffPicker,
    /// "Add to which session?" (docs/82 8.2): the hand-off picker's rows
    /// for a page's screenshot or notes when no composer is on screen. It
    /// holds what goes, so closing it unpicked attaches nothing anywhere.
    AttachPicker(Box<crate::browser::Parcel>),
    /// The Muse sign-in sheet.
    MuseSheet,
    /// A provider's setup sheet.
    SetupSheet(crate::setup::SetupSheet),
    /// The import sheet (Claude Code / Codex sessions).
    ImportSheet(crate::import_ui::ImportSheet),
    /// The Chrome import sheet.
    ChromeImport(crate::chrome_import_ui::ChromeImportSheet),
    /// A sidebar session row's inline rename: its session id.
    RenameSession(String),
    /// A project's inline rename: its project id.
    RenameProject(String),
    /// The Skills page header's Add menu.
    SkillsAddMenu,
    /// The Skills page detail's ⋯ menu.
    SkillsOverflow,
    // Opened only by a probe step.
    #[cfg_attr(not(feature = "probe"), allow(dead_code))]
    /// The context meter's breakdown, pinned open (a scripted capture's
    /// `meter` verb).
    Meter,
    /// The right pane's Create PR form: its base-branch menu.
    PrBaseMenu,
}

/// A [`Layer`] without its content: what `is_open` and `close` take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayerKind {
    Dialog,
    Palette,
    Menu,
    BrowseMenu,
    SiteInfo,
    Bookmark,
    Downloads,
    FilesMenu,
    PageMenu,
    AssistantMenu,
    EditorMenu,
    ConnectMenu,
    DefaultProviderMenu,
    HandoffPicker,
    AttachPicker,
    MuseSheet,
    SetupSheet,
    ImportSheet,
    ChromeImport,
    RenameSession,
    RenameProject,
    SkillsAddMenu,
    SkillsOverflow,
    Meter,
    PrBaseMenu,
}

impl Layer {
    /// The layer's kind.
    pub(crate) fn kind(&self) -> LayerKind {
        match self {
            Layer::Dialog(_) => LayerKind::Dialog,
            Layer::Palette(_) => LayerKind::Palette,
            Layer::Menu(_) => LayerKind::Menu,
            Layer::BrowseMenu(_) => LayerKind::BrowseMenu,
            Layer::SiteInfo => LayerKind::SiteInfo,
            Layer::Bookmark(_) => LayerKind::Bookmark,
            Layer::Downloads => LayerKind::Downloads,
            Layer::FilesMenu(_) => LayerKind::FilesMenu,
            Layer::PageMenu => LayerKind::PageMenu,
            Layer::AssistantMenu => LayerKind::AssistantMenu,
            Layer::EditorMenu => LayerKind::EditorMenu,
            Layer::ConnectMenu => LayerKind::ConnectMenu,
            Layer::DefaultProviderMenu => LayerKind::DefaultProviderMenu,
            Layer::HandoffPicker => LayerKind::HandoffPicker,
            Layer::AttachPicker(_) => LayerKind::AttachPicker,
            Layer::MuseSheet => LayerKind::MuseSheet,
            Layer::SetupSheet(_) => LayerKind::SetupSheet,
            Layer::ImportSheet(_) => LayerKind::ImportSheet,
            Layer::ChromeImport(_) => LayerKind::ChromeImport,
            Layer::RenameSession(_) => LayerKind::RenameSession,
            Layer::RenameProject(_) => LayerKind::RenameProject,
            Layer::SkillsAddMenu => LayerKind::SkillsAddMenu,
            Layer::SkillsOverflow => LayerKind::SkillsOverflow,
            Layer::Meter => LayerKind::Meter,
            Layer::PrBaseMenu => LayerKind::PrBaseMenu,
        }
    }

    /// Whether this layer, drawn in the main window, covers the native page.
    ///
    /// The covers are the ones the page has always hidden under: the dialog,
    /// the palette (and the attach picker, a modal like it), every menu but the sidebar footer's account menu (it
    /// stays inside the sidebar column), the two sign-in sheets. The rest draw where no page is (the pane's Files view,
    /// the Settings page, the sidebar) or in an overlay window over the live
    /// page. Browse's chrome popovers (downloads, extensions) move between
    /// the overlay window and the main window with the sidebar, so the view
    /// that draws them says what they cover, honestly, frame by frame
    /// (`BrowserView::occlude`), and they say nothing here.
    fn covers_page(&self) -> bool {
        match self {
            Layer::Dialog(_) | Layer::Palette(_) | Layer::AttachPicker(_) | Layer::MuseSheet | Layer::SetupSheet(_) => true,
            Layer::Menu(menu) => menu.kind != MenuKind::Account,
            _ => false,
        }
    }

    /// Whether this layer takes the main window's keyboard from what held
    /// it — and so gives it back when it closes. The composer's own menus
    /// (the caret menus and the chip pickers) leave the caret in the field;
    /// the popovers and row menus focus nothing of their own; and what an
    /// overlay window draws takes that window's keyboard, never the main
    /// window's. An inline rename types in its own field (opened from a
    /// menu row, it takes the keyboard over from the menu, see
    /// [`OverlayStack::push`]); it hands the keyboard to the composer itself
    /// when it ends, which a free-only restore leaves alone.
    fn takes_keyboard(&self) -> bool {
        match self {
            Layer::Dialog(_)
            | Layer::Palette(_)
            | Layer::AttachPicker(_)
            | Layer::MuseSheet
            | Layer::SetupSheet(_)
            | Layer::ImportSheet(_)
            | Layer::ChromeImport(_)
            | Layer::RenameSession(_)
            | Layer::RenameProject(_) => true,
            Layer::Menu(menu) => !matches!(
                menu.kind,
                MenuKind::Command | MenuKind::Mention | MenuKind::Model | MenuKind::Effort | MenuKind::Mode | MenuKind::Provider | MenuKind::Plus
            ),
            _ => false,
        }
    }

    /// Whether closing this layer always hands the keyboard back, even when
    /// something else looks focused: the modals and the menus focus an
    /// element of their own that unmounts with them, and the window goes on
    /// reporting it — or a handle the closing action focused that this frame
    /// never mounts — as focused. A keystroke against an unmounted handle
    /// skips every root action, so these never leave the keyboard there.
    /// The rest (a sheet, a rename) hand their own close to a deliberate
    /// focus, which a free-only restore leaves alone.
    fn reclaims_keyboard(&self) -> bool {
        matches!(self, Layer::Dialog(_) | Layer::Palette(_) | Layer::AttachPicker(_) | Layer::Menu(_))
    }

    /// Where this layer draws: the panels Browse floats over its page draw
    /// in an overlay window, the modals (the dialog, the palette, the attach
    /// picker) where the mode puts them, everything else in the main window.
    fn host(&self) -> Host {
        match self {
            Layer::BrowseMenu(_) | Layer::SiteInfo | Layer::Bookmark(_) | Layer::PageMenu | Layer::HandoffPicker => Host::Panel,
            Layer::Dialog(_) | Layer::Palette(_) | Layer::AttachPicker(_) => Host::Modal,
            _ => Host::Main,
        }
    }
}

/// One open overlay.
pub(crate) struct Entry {
    /// What it is, with its content.
    pub(crate) layer: Layer,
    /// Whether it covers the native page while it draws in the main window.
    pub(crate) covers_page: bool,
    /// Whether it takes the keyboard (see [`Layer::takes_keyboard`]).
    pub(crate) takes_keyboard: bool,
    /// Where it draws.
    pub(crate) host: Host,
    /// What held the keyboard in the frame it opened: closing it gives the
    /// keyboard back there.
    restore: Option<FocusHandle>,
    /// Where closing it gives the keyboard, whatever held it at open: the
    /// composer's menus hand it to the composer.
    restore_to: Option<FocusHandle>,
    /// The entity that draws it, notified when a pop (Escape, a view's
    /// dismissal) closes it.
    pub(crate) owner: Option<EntityId>,
    /// What Escape runs beyond dropping it.
    pub(crate) on_close: Option<OnClose>,
    /// The view that draws it, when it is its own view (a dialog, a sheet,
    /// a palette, a picker): the hosts draw this, and its
    /// [`gpui::DismissEvent`] pops the entry ([`Self::view`]).
    pub(crate) view: Option<AnyView>,
    /// The root's subscriptions to the view (its `DismissEvent`, its picks,
    /// its typed events), held as long as the entry: a closed entry hears
    /// nothing more.
    _kept: Vec<Subscription>,
}

impl Entry {
    /// An entry for `layer`, with its kind's cover, keyboard and host.
    pub(crate) fn new(layer: Layer) -> Self {
        Entry {
            covers_page: layer.covers_page(),
            takes_keyboard: layer.takes_keyboard(),
            host: layer.host(),
            layer,
            restore: None,
            restore_to: None,
            owner: None,
            on_close: None,
            view: None,
            _kept: Vec::new(),
        }
    }

    /// Hold `subscription` (to the entry's view) as long as the entry.
    pub(crate) fn keep(mut self, subscription: Subscription) -> Self {
        self._kept.push(subscription);
        self
    }

    /// Drawn by `view`. Its `DismissEvent` reaches the root through a
    /// subscription the root holds — kept on the entry
    /// (`Harness::push_overlay_view`), or the root's own for a view that
    /// outlives its entries (the setup sheet's host).
    pub(crate) fn view(mut self, view: AnyView) -> Self {
        self.view = Some(view);
        self
    }

    /// Give the keyboard to `focus` when it closes (and the keyboard is
    /// free), whatever held it when it opened.
    pub(crate) fn restore_to(mut self, focus: FocusHandle) -> Self {
        self.restore_to = Some(focus);
        self
    }

    /// Notify `owner` (the view that draws it) when Escape pops it.
    pub(crate) fn owner(mut self, owner: EntityId) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Run `close` when Escape pops it.
    pub(crate) fn on_close(mut self, close: impl Fn(Layer, &mut Harness, &mut Context<Harness>) + 'static) -> Self {
        self.on_close = Some(Rc::new(close));
        self
    }

    /// Its kind.
    pub(crate) fn kind(&self) -> LayerKind {
        self.layer.kind()
    }
}

/// What the root's settle does with the keyboard after an entry closed.
#[derive(Clone)]
pub(crate) struct Restore {
    /// Where the keyboard goes: what held it when the entry opened, or the
    /// root when nothing did.
    pub(crate) to: Option<FocusHandle>,
    /// What held the keyboard in the last frame the entry was open: still
    /// focused (or nothing, or the root) means the keyboard is free to go
    /// back; anything else means a click that closed the entry focused
    /// another field, which keeps it.
    pub(crate) held: Option<FocusHandle>,
    /// Hand the keyboard back even when it does not look free (see
    /// [`Layer::reclaims_keyboard`]).
    pub(crate) force: bool,
    /// Where the closed entry stood (0 is the oldest).
    depth: usize,
}

/// The window's open overlays, oldest first.
#[derive(Default)]
pub(crate) struct OverlayStack {
    entries: Vec<Entry>,
    /// What held the keyboard in the root's last frame (see
    /// [`OverlayStack::note_focus`]).
    focused: Option<FocusHandle>,
    /// The restore the root's settle applies.
    restore: Option<Restore>,
    /// The trigger whose entry the last outside press closed, and when
    /// ([`OverlayStack::note_pressed_shut`]).
    pressed_shut: Option<(Trigger, std::time::Instant)>,
}

/// A control that opens an entry and closes it again on a second click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// A menu's trigger in the main window (the sidebar's view options,
    /// account and row `⋯` buttons).
    Menu(MenuKind),
    /// The Browse page controls' ⋯, whose menu lives in the overlay window.
    BrowsePageMenu,
}

/// How soon after its press a click still counts as that press's own:
/// a click on the trigger of the entry the press just closed must not
/// reopen it.
pub(crate) const REPRESS: std::time::Duration = std::time::Duration::from_millis(400);

impl OverlayStack {
    /// Open `entry` on top, replacing an open entry of its kind. The entry
    /// remembers what held the keyboard in the last frame — unless it
    /// replaces an open entry (it keeps that one's), or takes over from one
    /// that closed this same frame (a palette row that opens a dialog): then
    /// it keeps what the closed one would have restored, and nothing is
    /// restored now.
    pub(crate) fn push(&mut self, mut entry: Entry) {
        let kind = entry.kind();
        let replaced = self.entries.iter().position(|open| open.kind() == kind).map(|at| self.entries.remove(at));
        entry.restore = match replaced {
            _ if entry.restore_to.is_some() => entry.restore_to.clone(),
            Some(old) => old.restore,
            None if entry.takes_keyboard => match self.restore.take() {
                Some(queued) => queued.to,
                None => self.focused.clone(),
            },
            None => self.focused.clone(),
        };
        self.entries.push(entry);
    }

    /// Close the entry of `kind`, if open, and queue its focus restore.
    pub(crate) fn close(&mut self, kind: LayerKind) -> Option<Entry> {
        let at = self.entries.iter().position(|open| open.kind() == kind)?;
        let entry = self.entries.remove(at);
        self.queue_restore(&entry, at);
        Some(entry)
    }

    /// Close the top entry (Escape), queue its focus restore, and return it:
    /// its owner to notify and its close hook ride on it.
    pub(crate) fn pop(&mut self) -> Option<Entry> {
        let entry = self.entries.pop()?;
        self.queue_restore(&entry, self.entries.len());
        Some(entry)
    }

    /// Queue the focus restore for `entry`, which stood at `depth` (0 is the
    /// oldest). Several entries closing in one frame (a mode switch, the
    /// Settings page opening) restore what the oldest of them took: each
    /// newer one took the keyboard from an entry that is gone too.
    fn queue_restore(&mut self, entry: &Entry, depth: usize) {
        if !entry.takes_keyboard && entry.restore_to.is_none() {
            return;
        }
        if self.restore.as_ref().is_some_and(|queued| queued.depth <= depth) {
            return;
        }
        let force = entry.takes_keyboard && entry.layer.reclaims_keyboard();
        self.restore = Some(Restore { to: entry.restore.clone(), held: self.focused.clone(), force, depth });
    }

    /// Close every entry of the `kinds`, as a mode switch does.
    pub(crate) fn close_all(&mut self, kinds: &[LayerKind]) {
        for kind in kinds {
            self.close(*kind);
        }
    }

    /// Whether an entry of `kind` is open.
    pub(crate) fn is_open(&self, kind: LayerKind) -> bool {
        self.entries.iter().any(|open| open.kind() == kind)
    }

    /// The top entry's kind.
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn top(&self) -> Option<LayerKind> {
        self.entries.last().map(Entry::kind)
    }

    /// The view drawing the entry of `kind`, when that entry is a view.
    pub(crate) fn view(&self, kind: LayerKind) -> Option<AnyView> {
        self.entry(kind).and_then(|open| open.view.clone())
    }

    /// The view drawing the entry of `kind`, as its own type.
    pub(crate) fn view_as<V: 'static>(&self, kind: LayerKind) -> Option<gpui::Entity<V>> {
        self.view(kind).and_then(|view| view.downcast::<V>().ok())
    }

    /// Whether an open entry is drawn by the view `id`.
    pub(crate) fn holds_view(&self, id: EntityId) -> bool {
        self.entries.iter().any(|open| open.view.as_ref().is_some_and(|view| view.entity_id() == id))
    }

    /// The entry of `kind`, if open.
    pub(crate) fn entry(&self, kind: LayerKind) -> Option<&Entry> {
        self.entries.iter().find(|open| open.kind() == kind)
    }

    /// The open entry of `kind`'s layer, to change its content.
    pub(crate) fn layer_mut(&mut self, kind: LayerKind) -> Option<&mut Layer> {
        self.entries.iter_mut().find(|open| open.kind() == kind).map(|open| &mut open.layer)
    }

    /// The open entry of `kind`'s layer.
    pub(crate) fn layer(&self, kind: LayerKind) -> Option<&Layer> {
        self.entry(kind).map(|open| &open.layer)
    }

    /// Open `layer` (`open`) or close its kind: a flag-shaped toggle.
    pub(crate) fn set(&mut self, layer: Layer, open: bool) {
        if open {
            self.push(Entry::new(layer));
        } else {
            self.close(layer.kind());
        }
    }

    /// Record that a press outside closed `trigger`'s entry. The press
    /// reaches the window before its click does, so when the press was on
    /// the entry's own trigger, the click that follows would reopen what the
    /// press just closed: the trigger asks [`Self::pressed_shut`] first.
    ///
    /// This is the one place that guard lives. It is a time guard because
    /// neither press says where it landed: the overlay window's resign (a
    /// press anywhere in the main window takes the keyboard back) and the
    /// library's backdrop press (`SidebarIntent::DismissMenus`) both arrive
    /// before the trigger's click, with no position.
    /// A press that closed nothing (`None`) clears the record.
    pub(crate) fn note_pressed_shut(&mut self, trigger: Option<Trigger>) {
        self.pressed_shut = trigger.map(|trigger| (trigger, std::time::Instant::now()));
    }

    /// How long ago a press closed `trigger`'s entry, taken once: `Some`
    /// only for the trigger the last press closed.
    pub(crate) fn pressed_shut(&mut self, trigger: Trigger) -> Option<std::time::Duration> {
        let (closed, at) = self.pressed_shut.take()?;
        (closed == trigger).then(|| at.elapsed())
    }

    /// Whether an open entry covers the native page in the current mode:
    /// one that says it does and draws in the main window, under the page.
    pub(crate) fn covers_page(&self, cx: &gpui::App) -> bool {
        self.covers_page_in(crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse)
    }

    /// [`Self::covers_page`] in Browse (`browse`) or Sessions.
    pub(crate) fn covers_page_in(&self, browse: bool) -> bool {
        self.entries.iter().any(|open| open.covers_page && open.host.resolve(browse) == Host::Main)
    }

    /// Whether the entry of `kind` is open and draws in an overlay window in
    /// the current mode (`browse`).
    pub(crate) fn hosted_in_panel(&self, kind: LayerKind, browse: bool) -> bool {
        self.entry(kind).is_some_and(|open| open.host.resolve(browse) == Host::Panel)
    }

    /// Record what held the keyboard this frame (the root's render): what the
    /// next entry to open will give it back to.
    pub(crate) fn note_focus(&mut self, focused: Option<FocusHandle>) {
        self.focused = focused;
    }

    /// The restore queued by the last close, once.
    pub(crate) fn take_restore(&mut self) -> Option<Restore> {
        self.restore.take()
    }
}

/// `$get` / `$get_mut`: the open `$variant` entry's content.
macro_rules! content {
    ($get:ident, $get_mut:ident, $variant:ident, $ty:ty) => {
        impl OverlayStack {
            #[doc = concat!("The open `", stringify!($variant), "` entry's content.")]
            pub(crate) fn $get(&self) -> Option<&$ty> {
                match self.layer(LayerKind::$variant) {
                    Some(Layer::$variant(content)) => Some(content),
                    _ => None,
                }
            }

            #[doc = concat!("The open `", stringify!($variant), "` entry's content, to change it.")]
            #[allow(dead_code)]
            pub(crate) fn $get_mut(&mut self) -> Option<&mut $ty> {
                match self.layer_mut(LayerKind::$variant) {
                    Some(Layer::$variant(content)) => Some(content),
                    _ => None,
                }
            }
        }
    };
}

content!(dialog, dialog_mut, Dialog, Dialog);
content!(palette, palette_mut, Palette, Palette);
content!(menu, menu_mut, Menu, Menu);
content!(browse_menu, browse_menu_mut, BrowseMenu, aui::screens::sidebar::BrowseMenu);
content!(bookmark, bookmark_mut, Bookmark, gpui::Entity<crate::overlay::BookmarkView>);
content!(files_menu, files_menu_mut, FilesMenu, crate::right_files::FilesMenu);
content!(setup_sheet, setup_sheet_mut, SetupSheet, crate::setup::SetupSheet);
content!(import_sheet, import_sheet_mut, ImportSheet, crate::import_ui::ImportSheet);
content!(chrome_import, chrome_import_mut, ChromeImport, crate::chrome_import_ui::ChromeImportSheet);
content!(renaming, renaming_mut, RenameSession, String);
content!(renaming_project, renaming_project_mut, RenameProject, String);

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;
    use aui::overlay::DialogKind;
    use crate::overlays::{DialogAction, PaletteKind};

    fn dialog() -> Layer {
        Layer::Dialog(Dialog {
            title: "x".into(),
            detail: "y".into(),
            kind: DialogKind::Error,
            primary: "Dismiss",
            action: DialogAction::Dismiss,
            archive_target: None,
        })
    }

    #[test]
    fn escape_pops_the_newest_entry_first() {
        let mut stack = OverlayStack::default();
        stack.push(Entry::new(dialog()));
        stack.push(Entry::new(Layer::Menu(Menu::picker(MenuKind::Model, 0))));
        assert_eq!(stack.pop().map(|entry| entry.kind()), Some(LayerKind::Menu));
        assert_eq!(stack.top(), Some(LayerKind::Dialog));
        assert_eq!(stack.pop().map(|entry| entry.kind()), Some(LayerKind::Dialog));
        assert!(stack.pop().is_none());
    }

    #[test]
    fn opening_a_kind_that_is_open_replaces_it_on_top() {
        let mut stack = OverlayStack::default();
        stack.push(Entry::new(Layer::Menu(Menu::picker(MenuKind::Model, 0))));
        stack.push(Entry::new(Layer::Palette(Palette { kind: PaletteKind::Commands })));
        stack.push(Entry::new(Layer::Menu(Menu::picker(MenuKind::Effort, 2))));
        assert_eq!(stack.top(), Some(LayerKind::Menu));
        assert_eq!(stack.menu().map(|menu| menu.kind), Some(MenuKind::Effort));
        stack.pop();
        assert!(!stack.is_open(LayerKind::Menu), "one menu at a time");
        assert_eq!(stack.top(), Some(LayerKind::Palette));
    }

    #[test]
    fn only_a_main_window_cover_covers_the_page() {
        let mut stack = OverlayStack::default();
        assert!(!stack.covers_page_in(false));
        stack.push(Entry::new(Layer::Menu(Menu::picker(MenuKind::Account, 0))));
        assert!(!stack.covers_page_in(false), "the account menu stays inside the sidebar");
        stack.push(Entry::new(dialog()));
        assert!(!stack.covers_page_in(true), "Browse hosts a dialog in the overlay window, above the live page");
        assert!(stack.covers_page_in(false), "Sessions draws the same dialog in the main window: it covers");
        stack.close(LayerKind::Dialog);
        stack.push(Entry::new(Layer::SiteInfo));
        assert!(!stack.covers_page_in(false), "a panel covers nothing");
    }
}
