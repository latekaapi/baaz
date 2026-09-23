//! Baaz's own keymap, as one table, plus the user file laid over it.
//!
//! Every shortcut the app binds lives in [`KEYMAP`]: the action's name, the
//! keystroke, the context it matches in, and — for the Settings UI a later
//! stage will build — the category and human label each binding belongs
//! beside, not in a second list that drifts. [`build_bindings`] turns the
//! table into the [`gpui::KeyBinding`]s [`crate::app::bind_keys`] installs,
//! so there is exactly one place that says what the app's shortcuts are.
//!
//! # The user file
//!
//! `~/Library/Application Support/baaz/keymap.json` ([`path`]), beside
//! `layout.json`, in Zed's shape: an array of `{context, bindings}` blocks,
//! where `bindings` maps a keystroke to an action name and `null` unbinds:
//!
//! ```json
//! [
//!   { "context": "AuiRoot", "bindings": { "cmd-t": "NewSession" } },
//!   { "context": null, "bindings": { "cmd-,": null } }
//! ]
//! ```
//!
//! A `null` (or missing, or empty) context is the global one, matching
//! [`KEYMAP`] rows with `context: None`. Any other context must name one of
//! the table's own contexts exactly — the tree is small on purpose
//! (docs/16-keymap.md §3.3) and a context that matches nothing would fail
//! silently, so it warns instead.
//!
//! [`load`] installs the defaults first and the accepted user bindings
//! second, so the user wins by gpui's existing depth-then-order rule. There
//! is deliberately no other override mechanism: the file is the source of
//! truth and the Settings UI (a sibling task) writes to it through
//! [`set_binding`], [`clear_binding`] and [`unbind_binding`], never to a
//! second store.
//!
//! # Load-time validation
//!
//! A hand-edited file must never stop the app, and a keymap whose failure is
//! invisible is worse than one that refuses (docs/16-keymap.md §3.2). Every
//! refused entry warns — collected in [`LoadedKeymap::warnings`] and logged
//! by [`crate::app::bind_keys`] through `baaz_log!`, the app's diagnostics
//! surface — and leaves the default installed:
//!
//! * unknown action → warn, keep the default;
//! * invalid context (not one of the table's) or unparseable keystroke → warn;
//! * duplicate `(keystroke, context)` in the user file → warn, keep the first;
//! * [`RESERVED`] keystroke → warn, keep the default. This is the guarantee
//!   the reserved list exists for: it refuses at load, not only in a UI that
//!   does not exist yet, so a hand-edited file cannot rebind ⌘Q.
//!
//! A missing file loads silently (there is nothing to report); a truncated
//! or unparseable one warns once and yields the defaults, per
//! [`crate::store`]'s best-effort-read rule. Under `BAAZ_DETERMINISTIC=1`
//! the user file is ignored entirely, exactly as `layout::read` returns the
//! defaults, so a personal keymap cannot move the Shortcuts capture.
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
            binding_for_action(entry.action, entry.keystroke, entry.context)
                .unwrap_or_else(|| panic!("keymap table names an unknown action: {}", entry.action))
        })
        .collect()
}

/// One [`gpui::KeyBinding`] for a table action name, or `None` when the name
/// is unknown. The table constructor ([`build_bindings`]) and the user-file
/// loader share this, so the two can never disagree about what an action is
/// called: the table panics on `None`, the loader warns on it.
fn binding_for_action(action: &str, keystroke: &str, context: Option<&str>) -> Option<gpui::KeyBinding> {
    match action {
        "SendTurn" => Some(gpui::KeyBinding::new(keystroke, crate::app::SendTurn, context)),
        "MenuConfirm" => Some(gpui::KeyBinding::new(keystroke, crate::app::MenuConfirm, context)),
        "ConfirmField" => Some(gpui::KeyBinding::new(keystroke, crate::app::ConfirmField, context)),
        "SteerTurn" => Some(gpui::KeyBinding::new(keystroke, crate::app::SteerTurn, context)),
        "MenuUp" => Some(gpui::KeyBinding::new(keystroke, crate::app::MenuUp, context)),
        "MenuDown" => Some(gpui::KeyBinding::new(keystroke, crate::app::MenuDown, context)),
        "HistoryPrev" => Some(gpui::KeyBinding::new(keystroke, crate::app::HistoryPrev, context)),
        "HistoryNext" => Some(gpui::KeyBinding::new(keystroke, crate::app::HistoryNext, context)),
        "SelectPrev" => Some(gpui::KeyBinding::new(keystroke, aui::keys::SelectPrev, context)),
        "SelectNext" => Some(gpui::KeyBinding::new(keystroke, aui::keys::SelectNext, context)),
        "PasteMaybeImage" => Some(gpui::KeyBinding::new(
            keystroke,
            crate::app::PasteMaybeImage,
            context,
        )),
        "AttachFile" => Some(gpui::KeyBinding::new(keystroke, crate::app::AttachFile, context)),
        "TogglePlan" => Some(gpui::KeyBinding::new(keystroke, crate::app::TogglePlan, context)),
        "Interrupt" => Some(gpui::KeyBinding::new(keystroke, crate::app::Interrupt, context)),
        "NewSession" => Some(gpui::KeyBinding::new(keystroke, crate::app::NewSession, context)),
        "AddProject" => Some(gpui::KeyBinding::new(keystroke, crate::app::AddProject, context)),
        "OpenModelMenu" => Some(gpui::KeyBinding::new(keystroke, crate::app::OpenModelMenu, context)),
        "OpenEffortMenu" => Some(gpui::KeyBinding::new(keystroke, crate::app::OpenEffortMenu, context)),
        "OpenModeMenu" => Some(gpui::KeyBinding::new(keystroke, crate::app::OpenModeMenu, context)),
        "FocusSearch" => Some(gpui::KeyBinding::new(keystroke, crate::app::FocusSearch, context)),
        "CloseWindow" => Some(gpui::KeyBinding::new(keystroke, crate::app::CloseWindow, context)),
        "QuitApp" => Some(gpui::KeyBinding::new(keystroke, crate::app::QuitApp, context)),
        "MinimizeWindow" => Some(gpui::KeyBinding::new(
            keystroke,
            crate::app::MinimizeWindow,
            context,
        )),
        "OpenSettings" => Some(gpui::KeyBinding::new(keystroke, crate::app::OpenSettings, context)),
        "ConfirmRename" => Some(gpui::KeyBinding::new(keystroke, crate::app::ConfirmRename, context)),
        "CopySelection" => Some(gpui::KeyBinding::new(keystroke, crate::app::CopySelection, context)),
        "ToggleTerminal" => Some(gpui::KeyBinding::new(keystroke, crate::app::ToggleTerminal, context)),
        "ToggleRightPane" => Some(gpui::KeyBinding::new(
            keystroke,
            crate::app::ToggleRightPane,
            context,
        )),
        "TerminalSigint" => Some(gpui::KeyBinding::new(keystroke, crate::app::TerminalSigint, context)),
        "NoAction" => Some(gpui::KeyBinding::new(keystroke, gpui::NoAction {}, context)),
        _ => None,
    }
}

/// A keystroke the user may not bind, in any context, to any action — not
/// through the Settings UI and not by hand-editing the file either.
pub struct ReservedBinding {
    /// The keystroke, in [`gpui::KeyBinding::new`] spelling (lowercase).
    pub keystroke: &'static str,
    /// Why it refuses: what the UI shows beside `editable: false`.
    pub reason: &'static str,
}

/// The bindings that refuse to be rebound: ⌘Q, ⌘W, ⌘H and the text editing
/// keys (docs/16-keymap.md §3.2). The defaults still bind some of these —
/// ⌘Q quits, ⌘W closes, ⌘C copies in the transcript — reserved means the
/// *user* cannot change what they do, not that the app does without them.
/// A user-file entry naming one warns and is dropped at load ([`load`]), and
/// [`set_binding`]/[`unbind_binding`] return [`KeymapError::Reserved`].
pub const RESERVED: &[ReservedBinding] = &[
    ReservedBinding {
        keystroke: "cmd-q",
        reason: "Quits the app; macOS expects ⌘Q to quit, in every context",
    },
    ReservedBinding {
        keystroke: "cmd-w",
        reason: "Closes the window; the red dot and ⌘W are the same act",
    },
    ReservedBinding {
        keystroke: "cmd-h",
        reason: "Hides the app; a macOS standard no window gives up",
    },
    ReservedBinding {
        keystroke: "cmd-z",
        reason: "Text editing; the focused field owns undo",
    },
    ReservedBinding {
        keystroke: "cmd-shift-z",
        reason: "Text editing; the focused field owns redo",
    },
    ReservedBinding {
        keystroke: "cmd-x",
        reason: "Text editing; the focused field owns cut",
    },
    ReservedBinding {
        keystroke: "cmd-c",
        reason: "Text editing; the focused field owns copy",
    },
    ReservedBinding {
        keystroke: "cmd-v",
        reason: "Text editing; the focused field owns paste",
    },
    ReservedBinding {
        keystroke: "cmd-a",
        reason: "Text editing; the focused field owns select-all",
    },
];

/// Whether the keystroke is [`RESERVED`], case-insensitively: `Cmd-Q` is ⌘Q.
pub fn is_reserved(keystroke: &str) -> bool {
    reserved_reason(keystroke).is_some()
}

/// Why the keystroke is [`RESERVED`], for the `editable: false` row.
pub fn reserved_reason(keystroke: &str) -> Option<&'static str> {
    let wanted = keystroke.trim().to_lowercase();
    RESERVED.iter().find(|reserved| reserved.keystroke == wanted).map(|reserved| reserved.reason)
}

/// `~/Library/Application Support/baaz/keymap.json`, beside `layout.json`.
pub fn path() -> std::path::PathBuf {
    crate::store::support_dir().join("keymap.json")
}

/// One `{context, bindings}` block of the user file. `bindings` maps a
/// keystroke to an action name; `null` unbinds that keystroke in the block's
/// context. A `null`, missing or empty `context` is the global one.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct UserBlock {
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    bindings: std::collections::BTreeMap<String, Option<String>>,
}

/// Whether the user file is ignored: `BAAZ_DETERMINISTIC=1`.
///
/// The same predicate `crate::clock::deterministic` checks, but read live
/// from the environment rather than once behind a `OnceLock`: the clock's
/// flag cannot be flipped back in-process, so a test could never turn a
/// deterministic load on, while this one can (under
/// [`crate::store::test_env_lock`]).
fn deterministic() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// What [`load`] installed: the defaults plus the accepted user bindings, in
/// the order gpui should see them, and one warning per refused entry.
pub struct LoadedKeymap {
    /// Defaults first, accepted user bindings second, so the user wins by
    /// gpui's existing depth-then-order rule. No other override mechanism.
    pub bindings: Vec<gpui::KeyBinding>,
    /// One line per refused user-file entry (unknown action, invalid
    /// context, bad keystroke, duplicate, reserved) plus one for a
    /// truncated or unparseable file. [`crate::app::bind_keys`] logs these
    /// through `baaz_log!`; the sibling Settings UI can show them as-is.
    pub warnings: Vec<String>,
}

/// Load the keymap: the built-in [`KEYMAP`] first, `keymap.json` second.
///
/// Never fails: a missing file is the defaults, a malformed one warns once
/// and is the defaults, and every refused entry warns and keeps the
/// default. Under `BAAZ_DETERMINISTIC=1` the user file is ignored entirely.
/// Blocking; call it off the UI thread.
pub fn load() -> LoadedKeymap {
    load_from_path(&path(), deterministic())
}

/// [`load`] against an explicit file, so tests never touch `HOME` or the
/// real `Application Support`. `ignore_user` forces the deterministic path.
pub(crate) fn load_from_path(path: &std::path::Path, ignore_user: bool) -> LoadedKeymap {
    let mut bindings = build_bindings();
    let mut warnings = Vec::new();
    if ignore_user {
        return LoadedKeymap { bindings, warnings };
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // Missing (or unreadable): the defaults, silently. There is nothing
        // to report about a file the person never wrote.
        Err(_) => return LoadedKeymap { bindings, warnings },
    };
    let blocks: Vec<UserBlock> = match serde_json::from_str(&text) {
        Ok(blocks) => blocks,
        Err(error) => {
            warnings.push(format!("keymap.json is malformed ({error}); using the built-in shortcuts"));
            return LoadedKeymap { bindings, warnings };
        }
    };
    let (accepted, mut refused) = resolve_user_blocks(&blocks);
    warnings.append(&mut refused);
    for entry in accepted {
        let context = entry.context.as_deref();
        match entry.action {
            // `null` unbinds: a `NoAction` binding loads after the default
            // for the same keystroke and context, wins the tie by order, and
            // consumes the keystroke — the same mechanism the `BaazTerminal`
            // rows use to hand keys to the grid.
            None => bindings.push(gpui::KeyBinding::new(&entry.keystroke, gpui::NoAction {}, context)),
            Some(action) => {
                // `resolve_user_blocks` only accepts known actions with valid
                // keystrokes, so this cannot fail; a failure would be a
                // validation hole, and losing the binding loudly beats
                // installing nothing silently.
                match binding_for_action(&action, &entry.keystroke, context) {
                    Some(binding) => bindings.push(binding),
                    None => warnings.push(format!(
                        "keymap.json entry for {action:?} did not build; keeping the built-in shortcuts"
                    )),
                }
            }
        }
    }
    LoadedKeymap { bindings, warnings }
}

/// One user-file entry that survived validation: rebind this keystroke in
/// this context to this action, or unbind it when `action` is `None`.
struct AcceptedBinding {
    keystroke: String,
    context: Option<String>,
    action: Option<String>,
}

/// The table's action names, including `NoAction` (which a user entry may
/// name explicitly to unbind, exactly like `null`).
fn known_action(name: &str) -> bool {
    KEYMAP.iter().any(|row| row.action == name)
}

/// The table's contexts: what a user block may name. `None` (a null, missing
/// or empty block context) is always valid — it is the global one.
fn known_context(context: Option<&str>) -> bool {
    match normalise_context(context) {
        None => true,
        Some(context) => KEYMAP.iter().any(|row| row.context == Some(context.as_str())),
    }
}

/// Lowercase and trim a user-written keystroke: `Cmd-Q` is ⌘Q, and gpui
/// spells every default lowercase.
fn normalise_keystroke(keystroke: &str) -> String {
    keystroke.trim().to_lowercase()
}

/// Trim a user-written context; empty means global.
fn normalise_context(context: Option<&str>) -> Option<String> {
    match context.map(str::trim) {
        None | Some("") => None,
        Some(context) => Some(context.to_string()),
    }
}

/// A user-written keystroke gpui can parse, as a single keystroke: chords
/// and sequences are not supported, and gpui's own parser decides.
fn valid_keystroke(keystroke: &str) -> bool {
    keystroke.split_whitespace().count() == 1 && gpui::Keystroke::parse(keystroke).is_ok()
}

/// Validate parsed user blocks in file order: the accepted entries plus one
/// warning per refused one. Pure, so the unit tests pin the rules without a
/// window; [`load_from_path`] only adds the file and the bindings.
fn resolve_user_blocks(blocks: &[UserBlock]) -> (Vec<AcceptedBinding>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut warnings = Vec::new();
    // Duplicate `(keystroke, context)` pairs in the user file: the first
    // wins, like the built-in table's test. The key is the normalised pair,
    // so `Cmd-Q` and `cmd-q` in one context collide rather than stack.
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for block in blocks {
        let context = normalise_context(block.context.as_deref());
        if !known_context(block.context.as_deref()) {
            warnings.push(format!(
                "keymap.json context {:?} is not one of the app's contexts; its {} {} ignored",
                block.context.as_deref().unwrap_or(""),
                block.bindings.len(),
                if block.bindings.len() == 1 { "binding is" } else { "bindings are" },
            ));
            continue;
        }
        let context_key = context.clone().unwrap_or_default();
        for (keystroke, action) in &block.bindings {
            let key = normalise_keystroke(keystroke);
            if is_reserved(&key) {
                warnings.push(format!(
                    "keymap.json entry for {key:?} is reserved ({}); keeping the built-in binding",
                    reserved_reason(&key).unwrap_or("reserved"),
                ));
                continue;
            }
            if !seen.insert((key.clone(), context_key.clone())) {
                warnings.push(format!(
                    "keymap.json binds {key:?} twice in the same context; keeping the first"
                ));
                continue;
            }
            match action {
                // `null` unbinds (§3.2), and so does the explicit `NoAction`
                // spelling the built-in table already uses: both mean "this
                // keystroke reaches no action in this context".
                None => accepted.push(AcceptedBinding {
                    keystroke: key,
                    context: context.clone(),
                    action: None,
                }),
                Some(name) if name == "NoAction" => accepted.push(AcceptedBinding {
                    keystroke: key,
                    context: context.clone(),
                    action: None,
                }),
                Some(name) if known_action(name) => {
                    if !valid_keystroke(&key) {
                        warnings.push(format!(
                            "keymap.json keystroke {keystroke:?} does not parse; its entry is ignored"
                        ));
                        continue;
                    }
                    accepted.push(AcceptedBinding {
                        keystroke: key,
                        context: context.clone(),
                        action: Some(name.clone()),
                    });
                }
                Some(name) => warnings.push(format!(
                    "keymap.json action {name:?} is unknown; keeping the built-in binding for {key:?}"
                )),
            }
        }
    }
    (accepted, warnings)
}

/// What setting, clearing or unbinding a binding can refuse. The sibling
/// Settings UI matches on this to say why a row will not take a keystroke.
#[derive(Debug, PartialEq, Eq)]
pub enum KeymapError {
    /// The keystroke is [`RESERVED`]: carries the keystroke and the reason.
    Reserved {
        keystroke: String,
        reason: String,
    },
    /// The action names no row of [`KEYMAP`].
    UnknownAction(String),
    /// The context is not one of the table's contexts.
    InvalidContext(String),
    /// The keystroke does not parse as a single keystroke.
    InvalidKeystroke(String),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::Reserved { keystroke, reason } => {
                write!(f, "{keystroke:?} is reserved: {reason}")
            }
            KeymapError::UnknownAction(action) => write!(f, "unknown action {action:?}"),
            KeymapError::InvalidContext(context) => write!(f, "invalid context {context:?}"),
            KeymapError::InvalidKeystroke(keystroke) => write!(f, "invalid keystroke {keystroke:?}"),
        }
    }
}

impl std::error::Error for KeymapError {}

/// Check one write the way the loader checks one entry, normalised.
fn check_write(
    action: &str,
    keystroke: &str,
    context: Option<&str>,
) -> Result<(String, String, Option<String>), KeymapError> {
    if !known_action(action) || action == "NoAction" {
        return Err(KeymapError::UnknownAction(action.to_string()));
    }
    if !known_context(context) {
        return Err(KeymapError::InvalidContext(context.unwrap_or("").to_string()));
    }
    let key = normalise_keystroke(keystroke);
    if !valid_keystroke(&key) {
        return Err(KeymapError::InvalidKeystroke(keystroke.to_string()));
    }
    if let Some(reason) = reserved_reason(&key) {
        return Err(KeymapError::Reserved {
            keystroke: key,
            reason: reason.to_string(),
        });
    }
    Ok((action.to_string(), key, normalise_context(context)))
}

/// Read the user file's blocks best-effort: missing or malformed is no
/// blocks, so a write never fails on a file the loader already tolerates.
/// (A malformed file is replaced by the write — its contents were already
/// refused at load, so nothing live is lost.)
fn read_blocks(path: &std::path::Path) -> Vec<UserBlock> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write blocks back through [`crate::store`]'s atomic rule: beside itself
/// and renamed.
fn write_blocks(path: &std::path::Path, blocks: &[UserBlock]) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(blocks).unwrap_or_else(|_| b"[]".to_vec());
    crate::store::write_atomic(path, &text)
}

/// Drop empty blocks so the file stays hand-readable.
fn prune_blocks(blocks: &mut Vec<UserBlock>) {
    blocks.retain(|block| !block.bindings.is_empty());
}

/// Set (or rebind) an action's binding in a context, writing the file.
/// Designed as if the Settings UI already existed: it validates exactly
/// what the loader validates — reserved keystrokes refuse here too, so the
/// UI and a hand-edited file cannot disagree.
///
/// Replacing moves the action: entries that would shadow the new one (the
/// same keystroke in the same context) or be shadowed by it (the action's
/// old keystrokes in the same context) are removed first, so the file never
/// holds a duplicate pair the loader would have to resolve.
pub fn set_binding(action: &str, keystroke: &str, context: Option<&str>) -> Result<(), KeymapError> {
    let (action, key, context) = check_write(action, keystroke, context)?;
    let path = path();
    let mut blocks = read_blocks(&path);
    let context_key = context.clone().unwrap_or_default();
    for block in &mut blocks {
        if normalise_context(block.context.as_deref()).unwrap_or_default() != context_key {
            continue;
        }
        block.bindings.retain(|existing_key, existing_action| {
            normalise_keystroke(existing_key) != key && *existing_action != Some(action.clone())
        });
    }
    match blocks.iter_mut().find(|block| {
        normalise_context(block.context.as_deref()).unwrap_or_default() == context_key
    }) {
        Some(block) => {
            block.bindings.insert(key, Some(action));
        }
        None => {
            let mut bindings = std::collections::BTreeMap::new();
            bindings.insert(key, Some(action));
            blocks.push(UserBlock { context, bindings });
        }
    }
    prune_blocks(&mut blocks);
    let _ = write_blocks(&path, &blocks);
    Ok(())
}

/// Forget an action's user-set bindings in a context, restoring the default.
/// Removes both rebinds and unbinds (`null`s at the action's default
/// keystrokes); a no-op when the file says nothing about the action. Only
/// restores, so nothing about it can be reserved.
pub fn clear_binding(action: &str, context: Option<&str>) -> Result<(), KeymapError> {
    if !known_action(action) || action == "NoAction" {
        return Err(KeymapError::UnknownAction(action.to_string()));
    }
    if !known_context(context) {
        return Err(KeymapError::InvalidContext(context.unwrap_or("").to_string()));
    }
    let context = normalise_context(context);
    let context_key = context.clone().unwrap_or_default();
    // The default keystrokes a `null` for this action could sit on.
    let defaults: Vec<String> = KEYMAP
        .iter()
        .filter(|row| row.action == action && row.context == context.as_deref())
        .map(|row| normalise_keystroke(row.keystroke))
        .collect();
    let path = path();
    let mut blocks = read_blocks(&path);
    let mut changed = false;
    for block in &mut blocks {
        if normalise_context(block.context.as_deref()).unwrap_or_default() != context_key {
            continue;
        }
        let before = block.bindings.len();
        block.bindings.retain(|existing_key, existing_action| {
            !(existing_action.as_deref() == Some(action)
                || (existing_action.is_none() && defaults.contains(&normalise_keystroke(existing_key))))
        });
        changed |= block.bindings.len() != before;
    }
    prune_blocks(&mut blocks);
    if changed {
        let _ = write_blocks(&path, &blocks);
    }
    Ok(())
}

/// Unbind an action in a context: write `null` at its current effective
/// keystroke. Refuses when that keystroke is [`RESERVED`] — ⌘Q stays bound —
/// and is a no-op when the action has no binding there.
pub fn unbind_binding(action: &str, context: Option<&str>) -> Result<(), KeymapError> {
    if !known_action(action) || action == "NoAction" {
        return Err(KeymapError::UnknownAction(action.to_string()));
    }
    if !known_context(context) {
        return Err(KeymapError::InvalidContext(context.unwrap_or("").to_string()));
    }
    let context = normalise_context(context);
    let Some(current) = effective_binding(action, context.as_deref()) else {
        return Ok(());
    };
    if let Some(reason) = reserved_reason(&current.keystroke) {
        return Err(KeymapError::Reserved {
            keystroke: current.keystroke,
            reason: reason.to_string(),
        });
    }
    let context_key = context.clone().unwrap_or_default();
    let path = path();
    let mut blocks = read_blocks(&path);
    for block in &mut blocks {
        if normalise_context(block.context.as_deref()).unwrap_or_default() != context_key {
            continue;
        }
        block.bindings.retain(|existing_key, _| normalise_keystroke(existing_key) != current.keystroke);
    }
    match blocks.iter_mut().find(|block| {
        normalise_context(block.context.as_deref()).unwrap_or_default() == context_key
    }) {
        Some(block) => {
            block.bindings.insert(current.keystroke, None);
        }
        None => {
            let mut bindings = std::collections::BTreeMap::new();
            bindings.insert(current.keystroke, None);
            blocks.push(UserBlock { context, bindings });
        }
    }
    prune_blocks(&mut blocks);
    let _ = write_blocks(&path, &blocks);
    Ok(())
}

/// One live binding: what the sibling Settings UI lists. A row with
/// `editable: false` is [`RESERVED`] and `reserved_reason` says why.
pub struct EffectiveBinding {
    /// The table action name.
    pub action: String,
    /// The keystroke that fires it, lowercase.
    pub keystroke: String,
    /// The context it fires in; `None` is global.
    pub context: Option<String>,
    /// The table grouping, for the UI's sections.
    pub category: String,
    /// The human label, for the UI's rows.
    pub label: String,
    /// False for reserved keystrokes: the row renders read-only.
    pub editable: bool,
    /// Why `editable` is false.
    pub reserved_reason: Option<String>,
}

/// Every live binding: the defaults overlaid with the user file, in that
/// order. A user entry for a `(keystroke, context)` the defaults hold
/// replaces the default row (it wins the tie at dispatch); `null` removes
/// it. `NoAction` rows are the terminal's implementation detail, not
/// shortcuts, so they are not listed.
pub fn effective_bindings() -> Vec<EffectiveBinding> {
    let mut rows: Vec<(String, String, Option<String>)> = KEYMAP
        .iter()
        .filter(|row| row.action != "NoAction")
        .map(|row| (row.action.to_string(), normalise_keystroke(row.keystroke), normalise_context(row.context)))
        .collect();
    let blocks = read_blocks(&path());
    let (accepted, _) = if deterministic() {
        (Vec::new(), Vec::new())
    } else {
        resolve_user_blocks(&blocks)
    };
    for entry in accepted {
        rows.retain(|(_, key, context)| {
            !(key == &entry.keystroke && context == &entry.context)
        });
        if let Some(action) = entry.action {
            rows.push((action, entry.keystroke, entry.context));
        }
    }
    rows.into_iter()
        .map(|(action, keystroke, context)| {
            let (category, label) = KEYMAP
                .iter()
                .find(|row| row.action == action)
                .map(|row| (row.category.to_string(), row.label.to_string()))
                .unwrap_or_default();
            let reserved_reason = reserved_reason(&keystroke).map(str::to_string);
            EffectiveBinding {
                action,
                keystroke,
                context,
                category,
                label,
                editable: reserved_reason.is_none(),
                reserved_reason,
            }
        })
        .collect()
}

/// The current effective binding for one action in one context: the last
/// live row, so a user rebind wins over the default. `None` when the action
/// is unbound there.
pub fn effective_binding(action: &str, context: Option<&str>) -> Option<EffectiveBinding> {
    let context = normalise_context(context);
    effective_bindings().into_iter().filter(|row| row.action == action && row.context == context).last()
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
