//! The composer's [`Picker`]s (plan 4.6): the model picker on the model
//! chip, the `@` file mention at the caret, and the new-session row's
//! folder picker (docs/82 6.6).
//!
//! Both keep the composer's caret: the draft holds the keyboard, and the
//! composer's own menu keys (↑ ↓ ← → ↩, see [`SessionView::move_menu`] and
//! [`SessionView::confirm_menu`]) drive the picker. The rows come from the
//! session each frame it draws (the catalog, the ranked files); the picker
//! keeps where the selection is and draws the library's component.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what the entity owns.

use super::*;
use crate::picker::{Picker, PickerDelegate, PickerFrame};

/// The model picker.
pub(crate) type ModelPicker = Picker<ModelDelegate>;
/// The `@` file mention.
pub(crate) type MentionPicker = Picker<MentionDelegate>;

/// The new-session row's folder picker.
pub(crate) type FolderPicker = Picker<FolderDelegate>;

/// What the new-session row shows (docs/82 6.6, docs/83 §5), set by the
/// window when an unsent draft activates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewRow {
    /// The folder control's text: the project's name, or the folder's
    /// last component for an unfiled draft.
    pub(crate) folder: String,
    /// The worktree toggle's state (`true` = the draft runs in its own
    /// worktree), or `None` where it cannot have one: not a git
    /// repository, no project, or a draft with no lane to move.
    pub(crate) worktree: Option<bool>,
}

/// The folder picker's "Choose a folder…" row id.
pub(crate) const CHOOSE_FOLDER_ROW: &str = "choose-folder";

/// The prefix of a folder-picker project row's id (`p:<project id>`).
pub(crate) const PROJECT_ROW_PREFIX: &str = "p:";

/// What a folder-picker pick asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderPick {
    /// Move the draft into this project.
    Project(String),
    /// The native folder panel, then the folder as a project.
    Choose,
}

impl FolderPick {
    /// The pick a row id names.
    pub(crate) fn from_row(id: &str) -> Option<Self> {
        if id == CHOOSE_FOLDER_ROW {
            return Some(Self::Choose);
        }
        id.strip_prefix(PROJECT_ROW_PREFIX).map(|project| Self::Project(project.to_owned()))
    }
}

/// The folder picker's rows, pushed by the window when it opens.
#[derive(Default)]
pub(crate) struct FolderDelegate {
    /// The adopted projects (the draft's first), then "Choose a folder…".
    pub(crate) rows: Vec<PickerRow>,
}

impl PickerDelegate for FolderDelegate {
    type Row = PickerRow;
    type Pick = FolderPick;

    fn match_count(&self) -> usize {
        self.rows.len()
    }

    fn match_id(&self, ix: usize) -> Option<SharedString> {
        self.rows.get(ix).map(|row| row.id.clone())
    }

    fn confirm(&self, ix: usize) -> Option<FolderPick> {
        self.match_id(ix).and_then(|id| FolderPick::from_row(&id))
    }

    fn confirm_id(&self, id: &SharedString) -> Option<FolderPick> {
        FolderPick::from_row(id)
    }

    fn render_match(&self, ix: usize, _selected: bool) -> PickerRow {
        self.rows[ix].clone()
    }

    fn render_picker(&self, rows: Vec<PickerRow>, frame: PickerFrame, _window: &mut Window, _cx: &mut Context<FolderPicker>) -> AnyElement {
        let (select, dismiss) = (frame.select.clone(), frame.dismiss.clone());
        let menu = aui::composer::folder_menu("folder-menu", rows, frame.selected, true)
            .on_pick(move |id, window, cx| select(id, window, cx))
            .on_close(move |window, cx| dismiss(window, cx));
        let menu = if crate::runtime::captures() { menu.at_rest() } else { menu };
        menu.into_any_element()
    }
}

/// What a model-picker pick asks for.
#[derive(Clone, Debug)]
pub(crate) enum ModelPick {
    /// A model row (or the "Models unavailable" reason row).
    Model(SharedString),
    /// A model and one of its effort levels, from the nested submenu.
    Nested(SharedString, SharedString),
}

/// The model picker's rows and state, pushed by the session each frame.
#[derive(Default)]
pub(crate) struct ModelDelegate {
    /// The catalog's rows, each with its effort submenu.
    pub(crate) rows: Vec<PickerRow>,
    /// The selected row's effort submenu stands open (→ opens it, ← shuts).
    pub(crate) sub_open: bool,
}

impl PickerDelegate for ModelDelegate {
    type Row = PickerRow;
    type Pick = ModelPick;

    fn match_count(&self) -> usize {
        self.rows.len()
    }

    fn match_id(&self, ix: usize) -> Option<SharedString> {
        self.rows.get(ix).map(|row| row.id.clone())
    }

    fn confirm(&self, ix: usize) -> Option<ModelPick> {
        self.match_id(ix).map(ModelPick::Model)
    }

    fn render_match(&self, ix: usize, _selected: bool) -> PickerRow {
        self.rows[ix].clone()
    }

    fn render_picker(&self, rows: Vec<PickerRow>, frame: PickerFrame, window: &mut Window, cx: &mut Context<ModelPicker>) -> AnyElement {
        let _ = window;
        let (select, dismiss) = (frame.select.clone(), frame.dismiss.clone());
        let me = cx.entity().downgrade();
        let nested = move |model: &SharedString, level: &SharedString, window: &mut Window, cx: &mut gpui::App| {
            let pick = ModelPick::Nested(model.clone(), level.clone());
            let _ = me.update(cx, |picker, cx| picker.pick(pick, window, cx));
        };
        let menu = model_menu("model-menu", rows, frame.selected, true)
            .align_end(true)
            .open_submenu(self.sub_open.then_some(frame.selected))
            .on_pick(move |id, window, cx| select(id, window, cx))
            .on_pick_nested(nested)
            .on_close(move |window, cx| dismiss(window, cx));
        let menu = if crate::runtime::captures() { menu.at_rest() } else { menu };
        menu.into_any_element()
    }
}

/// The `@` picker's rows, pushed by the session each frame.
#[derive(Default)]
pub(crate) struct MentionDelegate {
    /// The ranked paths for the filter.
    pub(crate) paths: Vec<String>,
    /// What has been typed after the `@`.
    pub(crate) filter: String,
    /// The rows' height cap: the room above the composer.
    pub(crate) max_rows: f32,
}

impl PickerDelegate for MentionDelegate {
    type Row = MentionItem;
    type Pick = String;

    fn match_count(&self) -> usize {
        self.paths.len()
    }

    fn update_matches(&mut self, query: &str) {
        query.clone_into(&mut self.filter);
    }

    fn match_id(&self, ix: usize) -> Option<SharedString> {
        self.paths.get(ix).map(|path| path.clone().into())
    }

    fn confirm(&self, ix: usize) -> Option<String> {
        self.paths.get(ix).cloned()
    }

    fn confirm_id(&self, id: &SharedString) -> Option<String> {
        Some(id.to_string())
    }

    fn render_match(&self, ix: usize, _selected: bool) -> MentionItem {
        let path = &self.paths[ix];
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        MentionItem::new(path.clone(), MentionIcon::Glyph(IconName::File), name, path.clone()).detail_mono().matching(&self.filter)
    }

    fn render_picker(&self, rows: Vec<MentionItem>, frame: PickerFrame, _window: &mut Window, _cx: &mut Context<MentionPicker>) -> AnyElement {
        let (select, hover) = (frame.select.clone(), frame.hover.clone());
        let picker = mention_picker("mention-picker", self.filter.clone(), vec![MentionSection::new("Files", rows)], frame.selected)
            .max_rows_height(self.max_rows);
        let picker = if crate::runtime::captures() { picker.at_rest() } else { picker };
        // The pointer moving over a row makes it the selection, so an arrow
        // continues from the row that is lit (the library only reports a
        // real move, so a resting pointer never takes the highlight).
        picker.on_select(move |id, window, cx| select(id, window, cx)).on_hover(move |ix, window, cx| hover(ix, window, cx)).into_any_element()
    }
}

impl SessionView {
    /// The open model picker.
    pub(crate) fn model_picker(&self, cx: &gpui::App) -> Option<Entity<ModelPicker>> {
        self.overlays.read(cx).stack.view_as::<ModelPicker>(crate::overlay_stack::LayerKind::Menu)
    }

    /// The open folder picker.
    pub(crate) fn folder_picker(&self, cx: &gpui::App) -> Option<Entity<FolderPicker>> {
        self.overlays.read(cx).stack.view_as::<FolderPicker>(crate::overlay_stack::LayerKind::Menu)
    }

    /// The open `@` picker.
    pub(crate) fn mention_picker(&self, cx: &gpui::App) -> Option<Entity<MentionPicker>> {
        self.overlays.read(cx).stack.view_as::<MentionPicker>(crate::overlay_stack::LayerKind::Menu)
    }

    /// Open `menu` (one of the composer's) drawn by `picker`: closing it
    /// hands the keyboard back to the composer, and its own dismissal (a
    /// press outside) closes it.
    fn open_picker_menu<D: PickerDelegate>(&mut self, menu: Menu, picker: Entity<Picker<D>>, cx: &mut Context<Self>) {
        let composer = self.composer_focus_handle(cx);
        let owner = cx.entity_id();
        let dismissed = cx.subscribe(&picker, |this: &mut Self, _, _: &gpui::DismissEvent, cx| this.close_menu(cx));
        let entry = crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::Menu(menu))
            .restore_to(composer)
            .owner(owner)
            .view(picker.into())
            .keep(dismissed);
        self.overlays.update(cx, |overlays, cx| overlays.push(entry, cx));
    }

    /// Open the model picker with the selection on `selected` (one past
    /// the last row is "no row": the chip names a model the catalog lacks).
    pub(super) fn open_model_picker(&mut self, selected: usize, cx: &mut Context<Self>) {
        let pick = cx.listener(|this: &mut Self, pick: &ModelPick, _, cx| this.on_model_pick(pick, cx));
        let delegate = ModelDelegate { rows: self.model_menu_rows(cx), sub_open: false };
        let picker = cx.new(|cx| {
            let mut picker = Picker::new(delegate, cx).on_pick(pick);
            picker.select(selected, cx);
            picker
        });
        self.open_picker_menu(Menu::picker(MenuKind::Model, selected), picker, cx);
    }

    /// Open the new-session row's folder picker on `rows` (the window
    /// lists them: the projects, the draft's first, then "Choose a
    /// folder…"), the selection on the first. A pick goes up to the window,
    /// which moves the draft.
    pub(crate) fn open_folder_picker(&mut self, rows: Vec<PickerRow>, cx: &mut Context<Self>) {
        let pick = cx.listener(|this: &mut Self, pick: &FolderPick, _, cx| {
            this.close_menu(cx);
            cx.emit(SessionEvent::FolderPicked { pick: pick.clone() });
        });
        let picker = cx.new(|cx| Picker::new(FolderDelegate { rows }, cx).on_pick(pick));
        self.open_picker_menu(Menu::picker(MenuKind::Folder, 0), picker, cx);
    }

    /// The folder control: closes its picker when open, else asks the
    /// window for the rows (it owns the projects).
    pub(crate) fn toggle_folder_picker(&mut self, cx: &mut Context<Self>) {
        if self.overlays.read(cx).is_open(MenuKind::Folder) {
            self.close_menu(cx);
        } else if self.runtime.read(cx).new_row().is_some() && !self.runtime.read(cx).is_moving() {
            cx.emit(SessionEvent::FolderMenu);
        }
    }

    #[cfg(feature = "probe")]
    /// `folder`: open the new-session row's folder picker. Fails when the
    /// session shows no row, the draft is moving, or the picker is open.
    pub(crate) fn step_folder(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if self.runtime.read(cx).new_row().is_none() {
            return Err("the open session shows no new-session row".into());
        }
        if self.runtime.read(cx).is_moving() {
            return Err("the draft is already moving".into());
        }
        if self.overlays.read(cx).is_open(MenuKind::Folder) {
            return Err("the folder picker is already open".into());
        }
        cx.emit(SessionEvent::FolderMenu);
        Ok(())
    }

    /// Open the `@` picker for the token at byte `at`, narrowed to `filter`.
    pub(super) fn open_mention_picker(&mut self, at: usize, filter: String, cx: &mut Context<Self>) {
        let pick = cx.listener(|this: &mut Self, path: &String, window, cx| {
            let insertion = format!("@{path} ");
            this.replace_token(&insertion, window, cx);
        });
        let query = filter.clone();
        let delegate = MentionDelegate { paths: self.mention_rows(&filter, cx), filter: filter.clone(), max_rows: 0.0 };
        let picker = cx.new(|cx| {
            let mut picker = Picker::new(delegate, cx).on_pick(pick);
            picker.set_query(&query, cx);
            picker
        });
        let mut menu = Menu::caret(MenuKind::Mention, at);
        menu.filter = filter;
        self.open_picker_menu(menu, picker, cx);
    }

    /// A model-picker pick: the model (the reason row restates why there is
    /// no catalog, then closes), or a model and its level in one press.
    fn on_model_pick(&mut self, pick: &ModelPick, cx: &mut Context<Self>) {
        match pick {
            ModelPick::Model(id) if id.as_ref() == MODEL_UNAVAILABLE_ROW => {
                if let Some(reason) = self.runtime.read(cx).models_error.clone() {
                    self.toast("Models unavailable", reason, cx);
                }
                self.close_menu(cx);
            }
            ModelPick::Model(id) => {
                let id = id.to_string();
                self.pick_model(&id, cx);
            }
            ModelPick::Nested(model, level) => {
                let effort = self
                    .rt(cx).effort_options_for(Some(model.as_ref()))
                    .options()
                    .and_then(|options| options.iter().find(|o| crate::overlays::effort_row_id(o.effort) == level.as_ref()).map(|o| o.effort));
                let model = model.to_string();
                self.pick_model(&model, cx);
                if let Some(effort) = effort {
                    self.pick_effort(effort, cx);
                }
            }
        }
    }
}
