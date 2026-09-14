//! The Settings dialog (⌘,, File → Settings…, the account menu's
//! "Settings…" row, `--steps settings[:<section>]`).
//!
//! The three sidebar flags stay in `layout.json` — there is no second store.
//! [`crate::app::Harness::settings_sections`] builds the dialog's sections
//! from that state every frame, and the dialog's `on_switch` intent flips
//! the matching layout field back through [`crate::layout::write`].
//!
//! Extensibility: a later section is one more arm in
//! [`crate::app::Harness::settings_sections`] and one more id in
//! [`crate::app::Harness::flip_setting`].

use aui::overlay::{popover_layer, settings_dialog, SettingsRow, SettingsSection};
use gpui::{prelude::*, AnyElement, Context, SharedString};

use crate::app::Harness;
use crate::overlays::Settings;

impl Harness {
    /// Every section of the Settings dialog, built from state each frame.
    ///
    /// A later section is one more arm here (and one more `on_switch` id in
    /// [`Self::flip_setting`]).
    pub(crate) fn settings_sections(&self) -> Vec<SettingsSection> {
        vec![SettingsSection {
            id: SharedString::from("sidebar"),
            label: SharedString::from("Sidebar"),
            rows: vec![
                SettingsRow::Switch {
                    id: SharedString::from("group_chevron"),
                    label: SharedString::from("Collapse chevron"),
                    detail: Some(SharedString::from("Show a chevron on project rows to fold them")),
                    on: self.layout.group_chevron,
                },
                SettingsRow::Switch {
                    id: SharedString::from("group_bar"),
                    label: SharedString::from("Current-project bar"),
                    detail: Some(SharedString::from("Mark the open session's project with an accent bar")),
                    on: self.layout.group_bar,
                },
                SettingsRow::Switch {
                    id: SharedString::from("group_branch"),
                    label: SharedString::from("Branch name"),
                    detail: Some(SharedString::from("Show each project's git branch on its row")),
                    on: self.layout.group_branch,
                },
            ],
        }]
    }

    /// Open the Settings dialog on `section`, closing whatever it covers.
    ///
    /// The dialog is the only overlay while open: menus, the palette and the
    /// plain modal go first, the way the archive dialog owns the overlay
    /// slot.
    pub(crate) fn open_settings(&mut self, section: usize, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| {
            overlays.menu = None;
            overlays.palette = None;
            overlays.dialog = None;
            overlays.settings = Some(Settings { section });
        });
        cx.notify();
    }

    /// Close the Settings dialog, if open.
    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.settings = None);
        cx.notify();
    }

    /// `settings[:<section>]`: open the dialog, optionally on the section
    /// with that id (`settings:sidebar`). An unknown id opens the first
    /// section.
    pub(crate) fn step_settings(&mut self, rest: &str, cx: &mut Context<Self>) {
        let sections = self.settings_sections();
        let section = settings_section_index(&sections, rest.trim());
        self.open_settings(section, cx);
    }

    /// Flip one Settings switch by its row id and persist it.
    ///
    /// Unknown ids are ignored: a later section adds its own arm here.
    pub(crate) fn flip_setting(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        match id {
            "group_chevron" => self.layout.group_chevron = on,
            "group_bar" => self.layout.group_bar = on,
            "group_branch" => self.layout.group_branch = on,
            _ => return,
        }
        crate::layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// The Settings dialog, rendered in the window root's overlay slot where
    /// the archive dialog renders.
    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.overlays.read(cx).settings.as_ref()?.section;
        let sections = self.settings_sections();
        let select = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            let index = *index;
            this.overlays.update(cx, |overlays, _| {
                if let Some(settings) = overlays.settings.as_mut() {
                    settings.section = index;
                }
            });
            cx.notify();
        });
        let flip = cx.listener(move |this: &mut Self, event: &(SharedString, bool), _, cx| {
            let (id, on) = event;
            this.flip_setting(id, *on, cx);
        });
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_settings(cx));
        let mut card = settings_dialog("settings", sections, selected)
            .on_select_section(move |i, w, cx| select(&i, w, cx))
            .on_switch(move |id, on, w, cx| flip(&(id.clone(), on), w, cx))
            .on_dismiss(move |w, cx| dismiss(&(), w, cx));
        if self.still() {
            card = card.at_rest();
        }
        Some(popover_layer(card).into_any_element())
    }
}

/// Which section `id` names: its index, or the first section when `id` is
/// empty or unknown. Pure, so tests can drive it without a window.
pub(crate) fn settings_section_index(sections: &[SettingsSection], id: &str) -> usize {
    if id.is_empty() {
        return 0;
    }
    sections.iter().position(|s| s.id.as_ref() == id).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections() -> Vec<SettingsSection> {
        vec![
            SettingsSection {
                id: SharedString::from("sidebar"),
                label: SharedString::from("Sidebar"),
                rows: Vec::new(),
            },
            SettingsSection {
                id: SharedString::from("appearance"),
                label: SharedString::from("Appearance"),
                rows: Vec::new(),
            },
        ]
    }

    #[test]
    fn an_empty_or_unknown_section_id_opens_the_first_section() {
        let sections = sections();
        assert_eq!(settings_section_index(&sections, ""), 0);
        assert_eq!(settings_section_index(&sections, "nope"), 0);
    }

    #[test]
    fn a_section_id_selects_its_section() {
        let sections = sections();
        assert_eq!(settings_section_index(&sections, "sidebar"), 0);
        assert_eq!(settings_section_index(&sections, "appearance"), 1);
    }
}
