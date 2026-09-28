//! The Settings dialog (⌘,, File → Settings…, the account menu's
//! "Settings…" row, `--steps settings[:<section>]`).
//!
//! The five sidebar flags stay in `layout.json` — there is no second store.
//! [`crate::app::Harness::settings_sections`] builds the dialog's sections
//! from that state every frame, and the dialog's `on_switch` intent flips
//! the matching layout field back through [`crate::layout::write`].
//!
//! Extensibility: a later section is one more arm in
//! [`crate::app::Harness::settings_sections`], one more id in
//! [`apply_setting`], and one more row below.
//!
//! The Shortcuts section is the exception to that shape: its rows come from
//! [`crate::keymap::effective_bindings`], cached on [`Harness`](crate::app::Harness)
//! (`shortcuts_cache`) and rebuilt per frame, and its edits go through
//! [`Harness::handle_shortcut`](crate::app::Harness::handle_shortcut).

use aui::overlay::{popover_layer, settings_dialog, SettingsRow, SettingsSection, ShortcutEdit};
use gpui::{prelude::*, AnyElement, Context, SharedString};

use crate::app::Harness;
use crate::layout::Layout;
use crate::overlays::Settings;

/// Flip one Sidebar switch in `layout` by row id. `false` is an unknown id,
/// ignored by every caller. Pure, so tests drive it without a window;
/// [`Harness::flip_setting`] and the `auto-*` step verbs persist the layout
/// and redraw around it.
pub(crate) fn apply_setting(layout: &mut Layout, id: &str, on: bool) -> bool {
    match id {
        "group_chevron" => layout.group_chevron = on,
        "group_bar" => layout.group_bar = on,
        "group_branch" => layout.group_branch = on,
        "auto_title" => layout.auto_title = on,
        "auto_summary" => layout.auto_summary = on,
        _ => return false,
    }
    true
}

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
                SettingsRow::Switch {
                    id: SharedString::from("auto_title"),
                    label: SharedString::from("Name sessions automatically"),
                    detail: Some(SharedString::from(
                        "Spend one cheap call naming a new session after its first message",
                    )),
                    on: self.layout.auto_title,
                },
                SettingsRow::Switch {
                    id: SharedString::from("auto_summary"),
                    label: SharedString::from("Summarise sessions in the sidebar"),
                    detail: Some(SharedString::from(
                        "Show the last request beside the last reply; rewrite poor ones",
                    )),
                    on: self.layout.auto_summary,
                },
            ],
        },
        providers_section(),
        shortcuts_section(
            &self.shortcuts_cache,
            self.recording_shortcut.as_deref(),
            &self.shortcut_errors,
        ),
        ]
    }

    /// Open the Settings dialog on the Providers section, closing whatever
    /// it covers: where the account menu's "Providers…" row lands.
    pub(crate) fn open_providers(&mut self, cx: &mut Context<Self>) {
        let section = settings_section_index(&self.settings_sections(), "providers");
        self.open_settings(section, cx);
    }

    /// Open the Settings dialog on `section`, closing whatever it covers.
    ///
    /// The dialog is the only overlay while open: menus, the palette and the
    /// plain modal go first, the way the archive dialog owns the overlay
    /// slot.
    pub(crate) fn open_settings(&mut self, section: usize, cx: &mut Context<Self>) {
        self.refresh_shortcuts();
        self.recording_shortcut = None;
        self.overlays.update(cx, |overlays, _| {
            overlays.menu = None;
            overlays.palette = None;
            overlays.dialog = None;
            overlays.settings = Some(Settings { section });
        });
        cx.notify();
    }

    /// Re-read the Shortcuts cache, so the section shows the file as it is
    /// now — after a write, or after a hand edit while the dialog stood open.
    fn refresh_shortcuts(&mut self) {
        self.shortcuts_cache = crate::keymap::effective_bindings();
    }

    /// A Shortcuts row edit, reported through the dialog's `on_shortcut`
    /// intent. `Record` arms the row — exactly one arms at a time; `Set`
    /// rebinds through [`crate::keymap::set_binding`] and reloads the live
    /// bindings, so the new key fires without a restart; `Clear` restores
    /// the default through [`crate::keymap::clear_binding`] and reloads;
    /// `Cancel` disarms. A refused write keeps the old binding and hangs
    /// the error text on the row's detail.
    pub(crate) fn handle_shortcut(&mut self, id: &SharedString, edit: &ShortcutEdit, cx: &mut Context<Self>) {
        match edit {
            ShortcutEdit::Record => {
                if shortcut_is_editable(id) {
                    self.recording_shortcut = Some(id.to_string());
                    self.shortcut_errors.remove(id.as_ref());
                }
            }
            ShortcutEdit::Set(keystroke) => {
                let accepted = parse_shortcut_row_id(id)
                    .map(|(action, context)| match crate::keymap::set_binding(&action, keystroke, context.as_deref())
                    {
                        Ok(()) => {
                            self.shortcut_errors.remove(id.as_ref());
                            true
                        }
                        Err(error) => {
                            self.shortcut_errors.insert(id.to_string(), error.to_string());
                            false
                        }
                    })
                    .unwrap_or(false);
                self.recording_shortcut = None;
                if accepted {
                    self.refresh_shortcuts();
                    crate::app::bind_keys(cx);
                }
            }
            ShortcutEdit::Clear => {
                let accepted = parse_shortcut_row_id(id)
                    .map(|(action, context)| match crate::keymap::clear_binding(&action, context.as_deref()) {
                        Ok(()) => {
                            self.shortcut_errors.remove(id.as_ref());
                            true
                        }
                        Err(error) => {
                            self.shortcut_errors.insert(id.to_string(), error.to_string());
                            false
                        }
                    })
                    .unwrap_or(false);
                self.recording_shortcut = None;
                if accepted {
                    self.refresh_shortcuts();
                    crate::app::bind_keys(cx);
                }
            }
            ShortcutEdit::Cancel => {
                if self.recording_shortcut.as_deref() == Some(id.as_ref()) {
                    self.recording_shortcut = None;
                }
            }
        }
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
    /// Unknown ids are ignored: a later section adds its own id to
    /// [`apply_setting`].
    pub(crate) fn flip_setting(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if !apply_setting(&mut self.layout, id, on) {
            return;
        }
        crate::layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// `auto-title`: flip the automatic-naming switch and persist it. The
    /// switch itself lives in the Settings dialog's Sidebar section; this
    /// verb is what captures flip. Off means no title model call ever.
    pub(crate) fn step_auto_title(&mut self, cx: &mut Context<Self>) {
        let on = !self.layout.auto_title;
        apply_setting(&mut self.layout, "auto_title", on);
        crate::layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// `auto-summary`: flip the sidebar-summaries switch and persist it.
    /// See [`Self::step_auto_title`]. Off means no rewrite model call ever;
    /// the ladder's preview rung still fills every second line.
    pub(crate) fn step_auto_summary(&mut self, cx: &mut Context<Self>) {
        let on = !self.layout.auto_summary;
        apply_setting(&mut self.layout, "auto_summary", on);
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
        let shortcut = cx.listener(
            move |this: &mut Self, event: &(SharedString, ShortcutEdit), _, cx| {
                let (id, edit) = event;
                this.handle_shortcut(id, edit, cx);
            },
        );
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_settings(cx));
        let mut card = settings_dialog("settings", sections, selected)
            .on_select_section(move |i, w, cx| select(&i, w, cx))
            .on_switch(move |id, on, w, cx| flip(&(id.clone(), on), w, cx))
            .on_shortcut(move |id, edit, w, cx| shortcut(&(id.clone(), edit.clone()), w, cx))
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

/// A Shortcuts row's stable id: the action and its context, joined on a
/// separator neither may contain, so [`parse_shortcut_row_id`] recovers both
/// for [`Harness::handle_shortcut`](crate::app::Harness::handle_shortcut).
pub(crate) fn shortcut_row_id(action: &str, context: Option<&str>) -> String {
    format!("{action}\u{1f}{}", context.unwrap_or(""))
}

/// The `(action, context)` a [`shortcut_row_id`] names. `None` is a corrupt
/// id, which the handler ignores rather than writing.
pub(crate) fn parse_shortcut_row_id(id: &str) -> Option<(String, Option<String>)> {
    let (action, context) = id.split_once('\u{1f}')?;
    if action.is_empty() {
        return None;
    }
    Some((
        action.to_string(),
        if context.is_empty() {
            None
        } else {
            Some(context.to_string())
        },
    ))
}

/// Whether the row may arm: it names a live binding that is not reserved.
/// The dialog never reports `Record` for a reserved row; this keeps a
/// scripted one honest too.
fn shortcut_is_editable(id: &str) -> bool {
    parse_shortcut_row_id(id).is_some_and(|(action, context)| {
        crate::keymap::effective_binding(&action, context.as_deref()).is_some_and(|binding| binding.editable)
    })
}

/// The Settings Providers section: one muted paragraph per backend under
/// a caps heading — the provider's name and its headline (`Connected ·
/// a@x.com · Ultra`, `Signed out`, …), read live from the status service
/// every frame. Read-only: switches and actions stay in the providers'
/// own surfaces. Pure, so tests drive it without a window.
pub(crate) fn providers_section() -> SettingsSection {
    let mut rows = vec![SettingsRow::Heading { text: SharedString::from("Connections") }];
    for status in crate::provider_status::live_statuses() {
        rows.push(SettingsRow::Note {
            text: SharedString::from(format!(
                "{} — {}",
                status.provider.label(),
                status.headline_text()
            )),
        });
    }
    SettingsSection { id: SharedString::from("providers"), label: SharedString::from("Providers"), rows }
}

/// The Settings Shortcuts section: one [`SettingsRow::Shortcut`] per live
/// binding, under a caps heading per keymap category, in table order — so a
/// rebound row stays beside its siblings instead of sinking to the end of
/// the effective list. The row label is the binding's label; the detail is
/// the last refusal on that row, else the reserved reason, else the context
/// when off-global. Pure, so tests drive it without a window.
pub(crate) fn shortcuts_section(
    bindings: &[crate::keymap::EffectiveBinding],
    recording: Option<&str>,
    errors: &std::collections::HashMap<String, String>,
) -> SettingsSection {
    let mut ordered: Vec<&crate::keymap::EffectiveBinding> = bindings.iter().collect();
    ordered.sort_by_key(|binding| {
        let category_at = crate::keymap::KEYMAP
            .iter()
            .position(|row| row.category == binding.category)
            .unwrap_or(usize::MAX);
        let action_at = crate::keymap::KEYMAP
            .iter()
            .position(|row| row.action == binding.action)
            .unwrap_or(usize::MAX);
        (category_at, action_at)
    });
    let mut rows = Vec::new();
    let mut last_category: Option<&str> = None;
    for binding in ordered {
        if last_category != Some(binding.category.as_str()) {
            rows.push(SettingsRow::Heading {
                text: SharedString::from(binding.category.clone()),
            });
            last_category = Some(binding.category.as_str());
        }
        let id = shortcut_row_id(&binding.action, binding.context.as_deref());
        let detail = if let Some(error) = errors.get(&id) {
            Some(SharedString::from(error.clone()))
        } else if !binding.editable {
            binding.reserved_reason.clone().map(SharedString::from)
        } else {
            binding.context.as_deref().and_then(context_label).map(SharedString::from)
        };
        rows.push(SettingsRow::Shortcut {
            id: SharedString::from(id.clone()),
            label: SharedString::from(binding.label.clone()),
            detail,
            keystroke: Some(SharedString::from(keystroke_glyphs(&binding.keystroke))),
            recording: recording == Some(id.as_str()),
            editable: binding.editable,
        });
    }
    SettingsSection {
        id: SharedString::from("shortcuts"),
        label: SharedString::from("Shortcuts"),
        rows,
    }
}

/// Where a binding fires, in words — or nothing for the global context,
/// which is the default and needs no caption. Internal gpui context names
/// ("AuiRoot", "BaazComposer && menu") are never shown to the person.
fn context_label(context: &str) -> Option<&'static str> {
    let context = context.trim();
    match context {
        "AuiRoot" | "" => None,
        c if c.starts_with("BaazComposer") && c.contains("histup") => Some("In the composer, on the first line"),
        c if c.starts_with("BaazComposer") && c.contains("histdown") => Some("In the composer, on the last line"),
        c if c.starts_with("BaazComposer") && c.contains("&& menu") => Some("In the composer, while a menu is open"),
        c if c.starts_with("BaazComposer") => Some("In the composer"),
        c if c.starts_with("BaazTerminal") => Some("In the terminal"),
        c if c.starts_with("BaazRename") => Some("While renaming"),
        c if c.starts_with("AuiMenu") => Some("In the palette"),
        _ => None,
    }
}

/// A keystroke as macOS writes it on a keycap: `cmd-shift-o` → `⌘⇧O`,
/// chords separated by a space. What is stored and matched stays gpui's
/// spelling; this is display only.
fn keystroke_glyphs(keystroke: &str) -> String {
    keystroke
        .split_whitespace()
        .map(|stroke| {
            let mut out = String::new();
            let parts: Vec<&str> = stroke.split('-').collect();
            let (mods, key) = match parts.split_last() {
                // `cmd--` (minus) splits into ["cmd", "", ""]
                Some((last, rest)) if last.is_empty() && !rest.is_empty() => (&rest[..rest.len() - 1], "-"),
                Some((last, rest)) => (rest, *last),
                None => (&parts[..0], ""),
            };
            for m in ["ctrl", "alt", "shift", "cmd"] {
                if mods.contains(&m) {
                    out.push_str(match m {
                        "ctrl" => "\u{2303}",
                        "alt" => "\u{2325}",
                        "shift" => "\u{21e7}",
                        _ => "\u{2318}",
                    });
                }
            }
            let key = match key {
                "enter" => "\u{21a9}".to_owned(),
                "escape" => "esc".to_owned(),
                "backspace" => "\u{232b}".to_owned(),
                "tab" => "\u{21e5}".to_owned(),
                "up" => "\u{2191}".to_owned(),
                "down" => "\u{2193}".to_owned(),
                "left" => "\u{2190}".to_owned(),
                "right" => "\u{2192}".to_owned(),
                "space" => "space".to_owned(),
                k if k.chars().count() == 1 => k.to_uppercase(),
                k => k.to_owned(),
            };
            out.push_str(&key);
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    /// Every Sidebar switch flips by row id, including the two auto
    /// switches — and an unknown id is ignored, never a new field by
    /// accident.
    #[test]
    fn every_sidebar_switch_flips_by_id() {
        let mut layout = Layout::default();
        assert!(apply_setting(&mut layout, "group_chevron", true));
        assert!(layout.group_chevron);
        assert!(apply_setting(&mut layout, "group_bar", true));
        assert!(layout.group_bar);
        assert!(apply_setting(&mut layout, "group_branch", true));
        assert!(layout.group_branch);
        assert!(apply_setting(&mut layout, "auto_title", false));
        assert!(!layout.auto_title);
        assert!(apply_setting(&mut layout, "auto_summary", false));
        assert!(!layout.auto_summary);
        assert!(!apply_setting(&mut layout, "nope", true));
    }

    /// A row id round-trips to the action and context the handler writes
    /// with — and a corrupt id parses to nothing, which the handler ignores.
    #[test]
    fn a_shortcut_row_id_round_trips_action_and_context() {
        assert_eq!(
            parse_shortcut_row_id(&shortcut_row_id("NewSession", Some("AuiRoot"))),
            Some(("NewSession".to_string(), Some("AuiRoot".to_string())))
        );
        assert_eq!(
            parse_shortcut_row_id(&shortcut_row_id("OpenSettings", None)),
            Some(("OpenSettings".to_string(), None))
        );
        assert_eq!(parse_shortcut_row_id("no-separator"), None);
        assert_eq!(parse_shortcut_row_id("\u{1f}AuiRoot"), None);
    }

    /// Holds [`crate::store::test_env_lock`] while a test points
    /// `BAAZ_STATE_DIR` at a fresh temp dir, restoring both after: the
    /// variables are process-global, so two such tests at once would read
    /// each other's state.
    struct ShortcutEnv {
        _guard: std::sync::MutexGuard<'static, ()>,
        state_dir: Option<std::ffi::OsString>,
        deterministic: Option<std::ffi::OsString>,
    }

    impl ShortcutEnv {
        fn hold(name: &str) -> (Self, std::path::PathBuf) {
            let env = Self {
                _guard: crate::store::test_env_lock(),
                state_dir: std::env::var_os("BAAZ_STATE_DIR"),
                deterministic: std::env::var_os("BAAZ_DETERMINISTIC"),
            };
            std::env::remove_var("BAAZ_DETERMINISTIC");
            let dir = std::env::temp_dir().join(format!("baaz-shortcuts-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("shortcuts test state dir");
            std::env::set_var("BAAZ_STATE_DIR", &dir);
            (env, dir)
        }
    }

    impl Drop for ShortcutEnv {
        fn drop(&mut self) {
            match &self.state_dir {
                Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
                None => std::env::remove_var("BAAZ_STATE_DIR"),
            }
            match &self.deterministic {
                Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
                None => std::env::remove_var("BAAZ_DETERMINISTIC"),
            }
        }
    }

    /// One [`SettingsRow::Shortcut`] per live binding, carrying its label:
    /// the section lists every default binding and nothing else, under a
    /// heading per category.
    #[test]
    fn the_shortcuts_section_lists_every_default_binding_with_its_label() {
        let (_env, _dir) = ShortcutEnv::hold("section");
        let bindings = crate::keymap::effective_bindings();
        assert!(!bindings.is_empty(), "the defaults list something");
        let section = shortcuts_section(&bindings, None, &std::collections::HashMap::new());
        assert_eq!(section.id.as_ref(), "shortcuts");
        let mut shortcuts = Vec::new();
        let mut headings = Vec::new();
        for row in &section.rows {
            match row {
                SettingsRow::Shortcut { id, label, keystroke, .. } => {
                    shortcuts.push((id.to_string(), label.to_string(), keystroke.clone()));
                }
                SettingsRow::Heading { text } => headings.push(text.to_string()),
                SettingsRow::Switch { .. } | SettingsRow::Note { .. } => {
                    panic!("the Shortcuts section holds only shortcut rows and headings");
                }
            }
        }
        assert_eq!(
            shortcuts.len(),
            bindings.len(),
            "one row per live binding, no more and no fewer"
        );
        for binding in &bindings {
            let id = shortcut_row_id(&binding.action, binding.context.as_deref());
            let (_, label, keystroke) = shortcuts
                .iter()
                .find(|(row_id, _, _)| row_id == &id)
                .unwrap_or_else(|| panic!("no row for {} in {:?}", binding.action, shortcuts));
            assert_eq!(label, &binding.label, "the row carries the binding's label");
            assert_eq!(
                keystroke.as_deref(),
                Some(keystroke_glyphs(&binding.keystroke).as_str()),
                "the row shows the live keystroke as keycaps"
            );
        }
        let mut categories: Vec<&str> = Vec::new();
        for binding in &bindings {
            if !categories.contains(&binding.category.as_str()) {
                categories.push(binding.category.as_str());
            }
        }
        assert_eq!(headings, categories, "one heading per category, in first-seen order");
    }

    /// Reserved rows are read-only with their reason as detail; an
    /// off-global row names its context; a global row has no detail; exactly
    /// one recording row arms.
    #[test]
    fn shortcut_rows_mark_recording_reserved_and_context() {
        let (_env, _dir) = ShortcutEnv::hold("rows");
        let bindings = crate::keymap::effective_bindings();
        let recording = shortcut_row_id("NewSession", Some("AuiRoot"));
        let section = shortcuts_section(&bindings, Some(&recording), &std::collections::HashMap::new());
        let mut saw_recording = 0;
        for row in &section.rows {
            if let SettingsRow::Shortcut { id, detail, recording: armed, editable, .. } = row {
                if *armed {
                    saw_recording += 1;
                    assert_eq!(id.as_ref(), recording, "only the named row arms");
                }
                let parsed = parse_shortcut_row_id(id).expect("every row id parses");
                let binding = bindings
                    .iter()
                    .find(|binding| {
                        binding.action == parsed.0 && binding.context == parsed.1
                    })
                    .expect("every row names a live binding");
                assert_eq!(*editable, binding.editable);
                if !binding.editable {
                    let reason = binding.reserved_reason.clone().expect("a reserved row says why");
                    assert_eq!(detail.as_deref(), Some(reason.as_str()), "reserved detail is the reason");
                } else if binding.action == "OpenSettings" {
                    assert_eq!(detail, &None, "a global row has no detail");
                } else if binding.action == "NewSession" {
                    assert_eq!(detail, &None, "the root context is global and says nothing");
                } else if let Some(context) = binding.context.as_deref() {
                    assert_eq!(detail.as_deref(), context_label(context), "contexts read as words");
                }
            }
        }
        assert_eq!(saw_recording, 1, "exactly one row arms at a time");
        let reserved = bindings.iter().find(|binding| !binding.editable).expect("a default is reserved");
        assert!(!reserved.reserved_reason.clone().unwrap_or_default().is_empty());
    }

    /// A refusal hangs on the row's detail, over whatever it showed before.
    #[test]
    fn a_shortcut_error_becomes_the_row_detail() {
        let (_env, _dir) = ShortcutEnv::hold("error");
        let bindings = crate::keymap::effective_bindings();
        let id = shortcut_row_id("NewSession", Some("AuiRoot"));
        let mut errors = std::collections::HashMap::new();
        errors.insert(id.clone(), "cmd-q is reserved".to_string());
        let section = shortcuts_section(&bindings, None, &errors);
        let detail = section.rows.iter().find_map(|row| match row {
            SettingsRow::Shortcut { id: row_id, detail, .. } if row_id.as_ref() == id => detail.clone(),
            _ => None,
        });
        assert_eq!(detail.as_deref(), Some("cmd-q is reserved"));
    }

    /// Off means no model call ever for that feature: the flipped layout
    /// straight into both pure decisions.
    #[test]
    fn a_switch_off_blocks_its_model_call() {
        let mut layout = Layout::default();
        apply_setting(&mut layout, "auto_title", false);
        assert!(!crate::titles::should_title(layout.auto_title, true, None, 0, false));
        apply_setting(&mut layout, "auto_summary", false);
        assert!(!crate::byline::should_rewrite(
            layout.auto_summary,
            false,
            true,
            (None, None),
            (Some("fix it"), None),
            None,
            std::time::Instant::now(),
        ));
    }

    #[test]
    fn keystrokes_read_as_keycaps() {
        // macOS order: control, option, shift, command.
        assert_eq!(keystroke_glyphs("cmd-shift-o"), "\u{21e7}\u{2318}O");
        assert_eq!(keystroke_glyphs("ctrl-c"), "\u{2303}C");
        assert_eq!(keystroke_glyphs("cmd-,"), "\u{2318},");
        assert_eq!(keystroke_glyphs("cmd--"), "\u{2318}-");
        assert_eq!(keystroke_glyphs("enter"), "\u{21a9}");
        assert_eq!(keystroke_glyphs("cmd-k cmd-s"), "\u{2318}K \u{2318}S");
    }

    #[test]
    fn contexts_read_as_words_and_the_global_one_is_silent() {
        assert_eq!(context_label("AuiRoot"), None);
        assert_eq!(context_label("AuiMenu > Input"), Some("In the palette"));
        assert_eq!(context_label("BaazComposer && menu"), Some("In the composer, while a menu is open"));
        assert_eq!(context_label("BaazRename"), Some("While renaming"));
        assert_eq!(context_label("SomethingNew"), None);
    }

}
