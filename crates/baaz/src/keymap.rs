//! Baaz's own keymap, as one table.
//!
//! Every shortcut the app binds lives in [`KEYMAP`]: the action's name, the
//! keystroke, the context it matches in, and — for the Settings UI a later
//! stage will build — the category and human label each binding belongs
//! beside, not in a second list that drifts. [`build_bindings`] turns the
//! table into the [`gpui::KeyBinding`]s [`crate::app::bind_keys`] installs,
//! so there is exactly one place that says what the app's shortcuts are.
//!
//! The terminal dock's grid runs under `BaazTerminal` (see
//! [`crate::app::TERMINAL_CONTEXT`]): every key reaches the pty except ⌃`,
//! ⌘K, ⌘B, ⌘W, ⌘Q and ⌘N (bound at the root, above the grid) and ⌘C with a
//! selection (which the grid copies itself). The `NoAction` rows below keep
//! the composer's Enter, paste and history keys — and the turn's ⌃C — from
//! firing while the dock holds the keyboard: `BaazTerminal` hangs deeper in
//! the tree than they do, so it wins, and `NoAction` consumes the keystroke
//! without an action, which hands it to the grid's own key handler.

/// One row of the keymap: everything a shortcut is, in one place.
pub(crate) struct KeymapEntry {
    /// The action's bare name, matched to its type in [`build_bindings`].
    pub(crate) action: &'static str,
    /// The keystroke, as [`gpui::KeyBinding::new`] parses it.
    pub(crate) keystroke: &'static str,
    /// The context the binding matches in; `None` matches anywhere.
    pub(crate) context: Option<&'static str>,
    /// The Settings grouping this shortcut will list under. Unused until
    /// that UI exists; it lives here so the grouping cannot drift from the
    /// binding.
    #[allow(dead_code)]
    pub(crate) category: &'static str,
    /// The human-readable name the Settings UI will show. Unused until
    /// then, for the same reason as `category`.
    #[allow(dead_code)]
    pub(crate) label: &'static str,
}

/// Every shortcut the app binds: (action, keystroke, context, category,
/// human label). The keystrokes and contexts are exactly the ones
/// `bind_keys` used to spell out one call at a time; only the shape changed.
pub(crate) const KEYMAP: &[KeymapEntry] = &[
    KeymapEntry { action: "SendTurn", keystroke: "enter", context: Some("BaazComposer && !menu && !field"), category: "composer", label: "Send turn" },
    KeymapEntry { action: "MenuConfirm", keystroke: "enter", context: Some("BaazComposer && menu"), category: "composer", label: "Confirm menu selection" },
    KeymapEntry { action: "ConfirmField", keystroke: "enter", context: Some("BaazComposer && field"), category: "composer", label: "Send card field" },
    KeymapEntry { action: "SteerTurn", keystroke: "cmd-enter", context: Some(crate::app::COMPOSER_CONTEXT), category: "composer", label: "Interject into running turn" },
    KeymapEntry { action: "MenuUp", keystroke: "up", context: Some("BaazComposer && menu"), category: "composer", label: "Menu selection up" },
    KeymapEntry { action: "MenuDown", keystroke: "down", context: Some("BaazComposer && menu"), category: "composer", label: "Menu selection down" },
    KeymapEntry { action: "HistoryPrev", keystroke: "up", context: Some("BaazComposer && histup && !menu"), category: "composer", label: "Previous prompt" },
    KeymapEntry { action: "HistoryNext", keystroke: "down", context: Some("BaazComposer && histdown && !menu"), category: "composer", label: "Next prompt" },
    KeymapEntry { action: "SelectPrev", keystroke: "up", context: Some(crate::app::PALETTE_QUERY_CONTEXT), category: "palette", label: "Palette selection up from the query field" },
    KeymapEntry { action: "SelectNext", keystroke: "down", context: Some(crate::app::PALETTE_QUERY_CONTEXT), category: "palette", label: "Palette selection down from the query field" },
    KeymapEntry { action: "PasteMaybeImage", keystroke: "cmd-v", context: Some(crate::app::COMPOSER_CONTEXT), category: "composer", label: "Paste, or attach an image" },
    KeymapEntry { action: "AttachFile", keystroke: "cmd-u", context: Some(crate::app::COMPOSER_CONTEXT), category: "composer", label: "Attach a file or photo" },
    KeymapEntry { action: "TogglePlan", keystroke: "shift-tab", context: Some(crate::app::COMPOSER_CONTEXT), category: "composer", label: "Toggle plan mode" },
    KeymapEntry { action: "Interrupt", keystroke: "ctrl-c", context: Some(aui::keys::ROOT_CONTEXT), category: "turn", label: "Stop the running turn" },
    KeymapEntry { action: "NewSession", keystroke: "cmd-n", context: Some(aui::keys::ROOT_CONTEXT), category: "session", label: "New session" },
    KeymapEntry { action: "AddProject", keystroke: "cmd-shift-o", context: Some(aui::keys::ROOT_CONTEXT), category: "project", label: "Add project" },
    KeymapEntry { action: "OpenModelMenu", keystroke: "cmd-shift-m", context: Some(aui::keys::ROOT_CONTEXT), category: "picker", label: "Open model picker" },
    KeymapEntry { action: "OpenEffortMenu", keystroke: "cmd-shift-e", context: Some(aui::keys::ROOT_CONTEXT), category: "picker", label: "Open reasoning-effort picker" },
    KeymapEntry { action: "OpenModeMenu", keystroke: "cmd-shift-p", context: Some(aui::keys::ROOT_CONTEXT), category: "picker", label: "Open approval-mode picker" },
    KeymapEntry { action: "FocusSearch", keystroke: "cmd-shift-f", context: Some(aui::keys::ROOT_CONTEXT), category: "search", label: "Focus sidebar search" },
    KeymapEntry { action: "CloseWindow", keystroke: "cmd-w", context: Some(aui::keys::ROOT_CONTEXT), category: "window", label: "Close window" },
    KeymapEntry { action: "QuitApp", keystroke: "cmd-q", context: Some(aui::keys::ROOT_CONTEXT), category: "window", label: "Quit app" },
    KeymapEntry { action: "MinimizeWindow", keystroke: "cmd-m", context: Some(aui::keys::ROOT_CONTEXT), category: "window", label: "Minimize window" },
    KeymapEntry { action: "OpenSettings", keystroke: "cmd-,", context: None, category: "settings", label: "Open settings" },
    KeymapEntry { action: "ConfirmRename", keystroke: "enter", context: Some(crate::app::RENAME_CONTEXT), category: "sidebar", label: "Confirm rename" },
    KeymapEntry { action: "CopySelection", keystroke: "cmd-c", context: Some(crate::session::TRANSCRIPT_COPY_KEYS), category: "transcript", label: "Copy selection" },
    KeymapEntry { action: "ToggleTerminal", keystroke: "ctrl-`", context: Some(aui::keys::ROOT_CONTEXT), category: "terminal", label: "Toggle terminal dock" },
    KeymapEntry { action: "ToggleRightPane", keystroke: "cmd-alt-b", context: Some(aui::keys::ROOT_CONTEXT), category: "pane", label: "Toggle right pane" },
    KeymapEntry { action: "TerminalSigint", keystroke: "ctrl-c", context: Some(crate::app::TERMINAL_CONTEXT), category: "terminal", label: "Interrupt terminal program" },
    KeymapEntry { action: "NoAction", keystroke: "enter", context: Some("BaazTerminal && !menu"), category: "terminal", label: "Terminal takes the key" },
    KeymapEntry { action: "NoAction", keystroke: "up", context: Some("BaazTerminal && !menu"), category: "terminal", label: "Terminal takes the key" },
    KeymapEntry { action: "NoAction", keystroke: "down", context: Some("BaazTerminal && !menu"), category: "terminal", label: "Terminal takes the key" },
    KeymapEntry { action: "NoAction", keystroke: "cmd-v", context: Some("BaazTerminal"), category: "terminal", label: "Terminal takes the key" },
    KeymapEntry { action: "NoAction", keystroke: "cmd-u", context: Some("BaazTerminal"), category: "terminal", label: "Terminal takes the key" },
    KeymapEntry { action: "NoAction", keystroke: "cmd-enter", context: Some("BaazTerminal"), category: "terminal", label: "Terminal takes the key" },
    KeymapEntry { action: "NoAction", keystroke: "shift-tab", context: Some("BaazTerminal"), category: "terminal", label: "Terminal takes the key" },
];

/// Builds one [`gpui::KeyBinding`] per [`KEYMAP`] row, in table order.
/// Panics on an unknown action name, so a row that names no real action
/// fails loudly instead of registering nothing.
pub(crate) fn build_bindings() -> Vec<gpui::KeyBinding> {
    KEYMAP
        .iter()
        .map(|entry| {
            let keystroke = entry.keystroke;
            let context = entry.context;
            match entry.action {
                "SendTurn" => gpui::KeyBinding::new(keystroke, crate::app::SendTurn, context),
                "MenuConfirm" => gpui::KeyBinding::new(keystroke, crate::app::MenuConfirm, context),
                "ConfirmField" => gpui::KeyBinding::new(keystroke, crate::app::ConfirmField, context),
                "SteerTurn" => gpui::KeyBinding::new(keystroke, crate::app::SteerTurn, context),
                "MenuUp" => gpui::KeyBinding::new(keystroke, crate::app::MenuUp, context),
                "MenuDown" => gpui::KeyBinding::new(keystroke, crate::app::MenuDown, context),
                "HistoryPrev" => gpui::KeyBinding::new(keystroke, crate::app::HistoryPrev, context),
                "HistoryNext" => gpui::KeyBinding::new(keystroke, crate::app::HistoryNext, context),
                "SelectPrev" => gpui::KeyBinding::new(keystroke, aui::keys::SelectPrev, context),
                "SelectNext" => gpui::KeyBinding::new(keystroke, aui::keys::SelectNext, context),
                "PasteMaybeImage" => gpui::KeyBinding::new(keystroke, crate::app::PasteMaybeImage, context),
                "AttachFile" => gpui::KeyBinding::new(keystroke, crate::app::AttachFile, context),
                "TogglePlan" => gpui::KeyBinding::new(keystroke, crate::app::TogglePlan, context),
                "Interrupt" => gpui::KeyBinding::new(keystroke, crate::app::Interrupt, context),
                "NewSession" => gpui::KeyBinding::new(keystroke, crate::app::NewSession, context),
                "AddProject" => gpui::KeyBinding::new(keystroke, crate::app::AddProject, context),
                "OpenModelMenu" => gpui::KeyBinding::new(keystroke, crate::app::OpenModelMenu, context),
                "OpenEffortMenu" => gpui::KeyBinding::new(keystroke, crate::app::OpenEffortMenu, context),
                "OpenModeMenu" => gpui::KeyBinding::new(keystroke, crate::app::OpenModeMenu, context),
                "FocusSearch" => gpui::KeyBinding::new(keystroke, crate::app::FocusSearch, context),
                "CloseWindow" => gpui::KeyBinding::new(keystroke, crate::app::CloseWindow, context),
                "QuitApp" => gpui::KeyBinding::new(keystroke, crate::app::QuitApp, context),
                "MinimizeWindow" => gpui::KeyBinding::new(keystroke, crate::app::MinimizeWindow, context),
                "OpenSettings" => gpui::KeyBinding::new(keystroke, crate::app::OpenSettings, context),
                "ConfirmRename" => gpui::KeyBinding::new(keystroke, crate::app::ConfirmRename, context),
                "CopySelection" => gpui::KeyBinding::new(keystroke, crate::app::CopySelection, context),
                "ToggleTerminal" => gpui::KeyBinding::new(keystroke, crate::app::ToggleTerminal, context),
                "ToggleRightPane" => gpui::KeyBinding::new(keystroke, crate::app::ToggleRightPane, context),
                "TerminalSigint" => gpui::KeyBinding::new(keystroke, crate::app::TerminalSigint, context),
                "NoAction" => gpui::KeyBinding::new(keystroke, gpui::NoAction {}, context),
                other => panic!("keymap table names an unknown action: {other}"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{build_bindings, KEYMAP};
    use std::collections::HashSet;

    /// Every action named in the table builds a binding for that same
    /// action, and every built binding comes from a table row: a new
    /// action without a keystroke fails here (unknown name panics, or the
    /// lengths disagree), and a dead keystroke cannot linger (it would
    /// build a binding no row accounts for).
    #[test]
    fn every_table_action_has_a_binding_and_vice_versa() {
        let bindings = build_bindings();
        assert_eq!(
            bindings.len(),
            KEYMAP.len(),
            "one binding per table row, no more and no fewer"
        );
        for (row, binding) in KEYMAP.iter().zip(bindings.iter()) {
            let built = binding.action().name().rsplit("::").next().unwrap_or("");
            assert_eq!(
                built, row.action,
                "row ({}, {}) built a binding for a different action",
                row.action, row.keystroke
            );
            assert_eq!(
                binding.keystrokes().len(),
                1,
                "row ({}, {}) should be a single keystroke",
                row.action,
                row.keystroke
            );
            assert_eq!(
                binding.predicate().is_some(),
                row.context.is_some(),
                "row ({}, {}) lost or gained its context",
                row.action,
                row.keystroke
            );
        }
    }

    /// No two rows claim the same keystroke in the same context: that is
    /// the conflict check, and the reason a table beats 36 loose calls.
    #[test]
    fn no_duplicate_keystroke_context_pairs() {
        let mut seen = HashSet::new();
        for row in KEYMAP {
            let pair = (row.keystroke, row.context.unwrap_or(""));
            assert!(
                seen.insert(pair),
                "duplicate keymap row: keystroke {:?} in context {:?}",
                row.keystroke,
                row.context
            );
        }
    }

    /// The Settings-stage metadata is filled in for every row, so no
    /// shortcut arrives there without a grouping and a label.
    #[test]
    fn every_row_has_a_category_and_label() {
        let categories: HashSet<&str> = [
            "composer", "turn", "session", "project", "picker", "palette", "search", "window", "settings",
            "sidebar", "transcript", "terminal", "pane",
        ]
        .into_iter()
        .collect();
        for row in KEYMAP {
            assert!(
                categories.contains(row.category),
                "row ({}, {}) names an unknown category {:?}",
                row.action,
                row.keystroke,
                row.category
            );
            assert!(
                !row.label.is_empty(),
                "row ({}, {}) has an empty human label",
                row.action,
                row.keystroke
            );
        }
    }
}
