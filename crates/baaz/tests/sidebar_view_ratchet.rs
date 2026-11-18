//! The sidebar as a real view (task 4.7, audit R6): the ratchet.
//!
//! The column used to be re-armed from the root: a per-frame hook built a
//! 30-field `SidebarKey` snapshot every frame and compared it
//! (`Harness::sync_sidebar_pane`), the pane rendered by reaching back into
//! the root (`baaz.update(|baaz, cx| baaz.render_sidebar(..))`), and the
//! wheel's input crossed through an `Rc<RefCell<SidebarWheelState>>` on
//! `Harness` because the pane could not update the root re-entrantly.
//!
//! Now `SidebarPane` owns its state and renders itself; it subscribes to the
//! `SessionList` model and observes the layout global; the root pushes the
//! facts only it knows (`compose_sidebar`) and maps the pane's events
//! (`on_sidebar_event`). The behaviour is pinned by gpui tests in
//! `sidebar_view.rs` (a list change redraws the column with no root notify;
//! a root notify with no change leaves the cached column alone). These pins
//! read source text so the old machinery cannot come back.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(manifest().join("src").join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every `.rs` file under `src`, with its path relative to `src`.
fn all_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("src reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let rel = path.strip_prefix(root).expect("under src").to_string_lossy().into_owned();
                out.push((rel, std::fs::read_to_string(&path).expect("source reads")));
            }
        }
    }
    let root = manifest().join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

/// Whether `text` names `word` as a whole identifier.
fn names(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// The body of `marker` in `text`: from it to the next closing brace at the
/// 4-space impl indent.
fn body<'a>(text: &'a str, marker: &str) -> &'a str {
    let start = text.find(marker).unwrap_or_else(|| panic!("`{marker}` exists"));
    let rest = &text[start..];
    let end = rest.find("\n    }\n").unwrap_or_else(|| panic!("`{marker}` ends"));
    &rest[..end]
}

/// The body of the struct `marker` opens: up to its closing brace.
fn struct_body<'a>(text: &'a str, marker: &str) -> &'a str {
    let start = text.find(marker).unwrap_or_else(|| panic!("`{marker}` exists"));
    let rest = &text[start..];
    let end = rest.find("\n}\n").unwrap_or_else(|| panic!("`{marker}` ends"));
    &rest[..end]
}

/// The code before the file's first `#[cfg(test)]` with comment lines
/// dropped: production code only (docs may name the root's methods).
fn production(text: &str) -> String {
    let code = text.find("\n#[cfg(test)]\nmod ").map_or(text, |at| &text[..at]);
    code.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// The frame key, its re-arm and the root's wheel cell are gone for good:
/// no file names them.
#[test]
fn the_frame_key_and_the_wheel_cell_are_gone() {
    for (rel, text) in all_sources() {
        for word in ["SidebarKey", "sync_sidebar_pane", "sidebar_key", "sidebar_wheel", "render_sidebar"] {
            assert!(!names(&text, word), "{rel} still names `{word}`");
        }
        assert!(
            !text.contains("RefCell<SidebarWheelState"),
            "{rel}: the wheel's state is the pane's own, never a shared cell"
        );
    }
}

/// The window's lifecycle (`app/window_sync.rs`, task 4.9: the settle, the
/// floats, the sweeps, the frame trace) neither re-arms, notifies, updates
/// nor feeds the column: no fact push, no sweep, no fade. (The frame trace
/// may read the list's top for its row: a diagnostic read, through
/// `sidebar_list_top`.)
#[test]
fn the_window_sync_does_not_touch_the_sidebar() {
    let sync = source("app/window_sync.rs");
    for word in ["self.sidebar_pane", "compose_sidebar", "push_sidebar_facts", "scroll_sweep(", "mode_fade", "SidebarPane"] {
        assert!(!sync.contains(word), "app/window_sync.rs names `{word}`");
    }
}

/// The pane holds no handle to the root: it renders itself from the list,
/// the layout global and the facts it was given, and asks through events.
#[test]
fn the_sidebar_view_holds_no_root_handle() {
    let view = source("sidebar_view.rs");
    let code = production(&view);
    for word in ["WeakEntity<Harness>", "Entity<Harness>", "use crate::app::", "baaz.update", "Harness::"] {
        assert!(!code.contains(word), "sidebar_view.rs production code names `{word}`");
    }
    let pane = struct_body(&code, "pub(crate) struct SidebarPane {");
    assert!(!pane.contains("Harness"), "the pane's fields name the root");
    assert!(
        code.contains("cx.subscribe(&list,") && code.contains("observe_global::<crate::layout::LayoutStore>"),
        "the pane subscribes to the session list and observes the layout global"
    );
    assert!(
        code.contains("impl EventEmitter<SidebarEvent> for SidebarPane"),
        "the pane asks the root through typed events"
    );
}

/// The list notifies itself, never the root: its change sites emit a model
/// event and notify the list entity, and the root's list fields live on it.
#[test]
fn the_session_list_is_a_model_that_says_what_changed() {
    let list = source("session_list.rs");
    let invalidate = body(&list, "pub(crate) fn invalidate(");
    assert!(invalidate.contains("Self::say(SessionListEvent::Changed, cx)"), "invalidate says what changed");
    let row = body(&list, "pub(crate) fn invalidate_row(");
    assert!(row.contains("Self::say(SessionListEvent::RowChanged(id), cx)"), "a one-row change says which row");
    let say = body(&list, "fn say(");
    assert!(
        say.contains("cx.emit(event)") && say.contains("cx.notify()"),
        "saying a change emits the model event and notifies the list itself"
    );
    let app = source("app.rs");
    let harness = struct_body(&app, "pub struct Harness {");
    // The field names, one per `name:` line at the struct's own indent.
    let fields: Vec<&str> = harness
        .lines()
        .filter(|line| line.starts_with("    ") && !line.starts_with("     "))
        .map(|line| line.trim_start().trim_start_matches("pub(crate) ").trim_start_matches("pub "))
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .filter(|name| name.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .collect();
    for field in [
        "sessions",
        "list_epoch",
        "overrides",
        "provider_sessions",
        "titles_pending",
        "show_hidden",
        "reveal",
        "sidebar_list",
        "hovered_row",
        "mode_fade_since",
    ] {
        assert!(!fields.contains(&field), "Harness still holds `{field}`");
    }
    assert!(fields.contains(&"session_list") && fields.contains(&"sidebar_pane"), "the root holds the two handles");
}
