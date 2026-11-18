//! Render composes (task 4.9, audit A F19): the ratchet.
//!
//! `Harness::render` used to run a per-frame hook first: it opened, moved
//! and closed the overlay windows, set the window title, settled a resize
//! drag, ran the scripted boot, moved the keyboard, warmed the browser and
//! paced the sweeps — and `render_centre` opened a `--replay` capture (a
//! fold, a row written to `sessions.json`, the search index rebuilt). A
//! quiet window took none of it; every frame paid for all of it, ordered by
//! where in the element tree it happened to sit. Each duty now runs from
//! what causes it (`app/window_sync.rs`), and render reads state and builds
//! elements.
//!
//! These pins read source text. Render, and every function in `app.rs` /
//! `app/**/*.rs` it reaches through `self.` / `Self::` calls, must not
//! write a file, open a window, move the keyboard, touch sqlite or set the
//! window title. The reachable set is computed from the source and must be
//! covered by [`RENDER_HELPERS`], the explicit list of the root's render
//! helpers, so a new helper is named here before it can draw; and every
//! `fn render_*` / `fn compose_*` in those files is held to the same rule
//! whether render reaches it or not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `app.rs` and every file under `app/` except tests, as `(path, text)`.
fn app_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src/app reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "tests") {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = vec![src().join("app.rs")];
    walk(&src().join("app"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            (path.strip_prefix(src()).expect("under src").display().to_string(), strip_tests(&strip_comments(&text)))
        })
        .collect()
}

/// The text without `//` comments (doc comments included): what the code
/// says, not what its prose mentions.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text before an inline `mod tests` block: test helpers may do
/// anything.
fn strip_tests(text: &str) -> String {
    match text.find("#[cfg(test)]\nmod tests") {
        Some(at) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

/// Every `fn name` in `text` with its body: from the `fn` line to the
/// closing brace at that line's indent.
fn functions(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (at, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let decl = ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn ", "pub(in crate::app) fn "]
            .iter()
            .find_map(|lead| trimmed.strip_prefix(lead));
        let Some(rest) = decl else { continue };
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if name.is_empty() {
            continue;
        }
        let close = format!("{}}}", " ".repeat(indent));
        let end = lines[at..].iter().position(|l| *l == close).map(|n| at + n).unwrap_or(lines.len() - 1);
        out.push((name, lines[at..=end].join("\n")));
    }
    out
}

/// The `self.name(` / `Self::name(` calls in `body`.
fn self_calls(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for lead in ["self.", "Self::"] {
        let mut rest = body;
        while let Some(at) = rest.find(lead) {
            rest = &rest[at + lead.len()..];
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if rest[name.len()..].starts_with('(') && !name.is_empty() {
                out.insert(name);
            }
        }
    }
    out
}

/// What no frame may do, by the text that does it.
const FORBIDDEN: &[&str] = &[
    // writes a file
    "std::fs::write",
    "fs::write(",
    "File::create",
    // opens a window
    "open_window",
    "NSPanel",
    "overlay::open",
    // moves the keyboard
    ".focus(",
    "focus_handle.focus(",
    "window.focus(",
    // touches sqlite
    "rusqlite",
    ".execute(",
    // sets the window title
    "set_window_title",
];

/// What in `body` is forbidden: the words above, `Connection` as a word,
/// and any call whose name says it persists or saves.
fn offences(body: &str) -> Vec<String> {
    let mut found: Vec<String> = FORBIDDEN.iter().filter(|word| body.contains(*word)).map(|word| (*word).to_owned()).collect();
    let mut ident = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            ident.push(c);
            if chars.peek().is_some_and(|next| next.is_alphanumeric() || *next == '_') {
                continue;
            }
            let call = chars.peek() == Some(&'(');
            if ident == "Connection" || (call && (ident.contains("persist") || ident.contains("save"))) {
                found.push(ident.clone());
            }
        }
        ident.clear();
    }
    found
}

/// The root's render helpers in `app.rs` / `app/**`: what `Harness::render`
/// composes from, directly or through each other. A helper render reaches
/// that is missing here fails [`render_reaches_only_listed_helpers`].
const RENDER_HELPERS: &[&str] = &[
    "compose_sidebar",
    "compose_terminal_dock",
    "render_bar_chrome",
    "render_browse_centre",
    "render_browse_crashed",
    "render_browse_downloads",
    "render_browse_empty",
    "render_browse_extensions",
    "render_browse_find",
    "render_browse_more",
    "render_browse_newtab",
    "render_browse_permission",
    "render_centre",
    "render_no_session",
    "render_omnibox_dropdown",
    "render_peek",
    "render_pending_switch",
    "render_provider_open_error",
    "render_row_detail",
    "render_session_title",
    "render_sidebar_toggle",
    "render_split",
    "render_wire_banner",
];

/// The root's render body.
fn render_body(app: &[(String, String)]) -> String {
    let text = &app.iter().find(|(path, _)| path == "app.rs").expect("app.rs").1;
    let start = text.find("impl Render for Harness {").expect("Harness renders");
    let rest = &text[start..];
    let end = rest.find("\n    }\n").expect("render ends");
    rest[..end].to_owned()
}

/// Every function render reaches through `self.` / `Self::` calls, by name,
/// with every body that name has in `app.rs` / `app/**`.
fn reachable(app: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut bodies: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, text) in app {
        for (name, body) in functions(text) {
            bodies.entry(name).or_default().push(body);
        }
    }
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut todo = vec![render_body(app)];
    while let Some(body) = todo.pop() {
        for name in self_calls(&body) {
            if seen.contains_key(&name) {
                continue;
            }
            if let Some(found) = bodies.get(&name) {
                seen.insert(name.clone(), found.clone());
                todo.extend(found.iter().cloned());
            }
        }
    }
    seen
}

/// Render itself, and everything it reaches in the root's files, writes no
/// file, opens no window, moves no keyboard, touches no sqlite and sets no
/// title.
#[test]
fn render_and_what_it_reaches_compose_only() {
    let app = app_sources();
    let render = render_body(&app);
    assert!(render.contains("self.trace_root_frame(cx);"), "render keeps its one instrument");
    let found = offences(&render);
    assert!(found.is_empty(), "Harness::render does {found:?}: that belongs to the event that causes it");
    let reached = reachable(&app);
    assert!(reached.len() > 20, "the reachable set was computed (got {})", reached.len());
    for (name, bodies) in &reached {
        for body in bodies {
            let found = offences(body);
            assert!(found.is_empty(), "`{name}`, reached from Harness::render, does {found:?}");
        }
    }
}

/// The reachable render helpers are the listed ones: a new `render_*` or
/// `compose_*` that render reaches is named in [`RENDER_HELPERS`] first.
/// Every listed one exists and is reached.
#[test]
fn render_reaches_only_listed_helpers() {
    let app = app_sources();
    let reached = reachable(&app);
    let helpers: BTreeSet<&str> =
        reached.keys().map(String::as_str).filter(|name| name.starts_with("render_") || name.starts_with("compose_")).collect();
    let listed: BTreeSet<&str> = RENDER_HELPERS.iter().copied().collect();
    let unlisted: Vec<&&str> = helpers.difference(&listed).collect();
    assert!(unlisted.is_empty(), "render reaches helpers missing from RENDER_HELPERS: {unlisted:?}");
    let unreached: Vec<&&str> = listed.difference(&helpers).collect();
    assert!(unreached.is_empty(), "RENDER_HELPERS lists helpers render no longer reaches: {unreached:?}");
}

/// Every `fn render_*` / `fn compose_*` in the root's files — reached from
/// render or drawn from elsewhere (a picker delegate, the overlay window) —
/// is held to the same rule.
#[test]
fn every_root_render_helper_composes_only() {
    let app = app_sources();
    let mut checked = 0;
    for (path, text) in &app {
        for (name, body) in functions(text) {
            if !(name.starts_with("render_") || name.starts_with("compose_")) {
                continue;
            }
            checked += 1;
            let found = offences(&body);
            assert!(found.is_empty(), "{path}: `{name}` does {found:?}");
        }
    }
    assert!(checked >= RENDER_HELPERS.len(), "the helpers were found (got {checked})");
}

/// The per-frame hook is gone, in code and in prose: nothing under `src/`
/// names it (gpui's own `on_next_frame` and `request_animation_frame`
/// aside).
#[test]
fn the_per_frame_hook_is_gone() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&src(), &mut files);
    for path in files {
        let text = std::fs::read_to_string(&path).expect("source reads");
        let named = text.match_indices("on_frame").any(|(at, _)| {
            !text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
        assert!(!named, "{} names `on_frame`", path.display());
    }
}

/// The duties went to their causes: the settle runs from observations made
/// once at boot, the floats a frame after the change, the title from the
/// list's events, and activation from the window's own observer.
#[test]
fn the_duties_follow_their_causes() {
    let sync = std::fs::read_to_string(src().join("app/window_sync.rs")).expect("window_sync.rs reads");
    let install = functions(&sync).into_iter().find(|(name, _)| name == "install_window_sync").expect("install exists").1;
    for word in [
        "cx.observe(&root,",
        "cx.observe(&self.overlays,",
        "cx.observe(&self.browser,",
        "cx.observe_global::<layout::LayoutStore>",
        "cx.observe_window_bounds(window,",
        "cx.observe_window_activation(window,",
        "cx.on_focus_lost(window,",
        "SessionListEvent",
    ] {
        assert!(install.contains(word), "install_window_sync wires `{word}`");
    }
    let floats = functions(&sync).into_iter().find(|(name, _)| name == "queue_floats").expect("queue_floats exists").1;
    assert!(floats.contains("cx.on_next_frame(window,"), "the floats sync after the layout that moved their anchors");
    let app = std::fs::read_to_string(src().join("app.rs")).expect("app.rs reads");
    assert!(app.contains("this.install_window_sync(window, cx);"), "the root wires its lifecycle when it is built");
    assert!(
        app.contains("cx.defer_in(window, |this, window, cx| this.open_replay(window, cx));"),
        "a replay opens when the root is built, not from a frame"
    );
    for gone in ["window_title_key", "transcript_scroll_sweep:", "focus_composer: bool,\n    /// What the billing"] {
        assert!(!app.contains(gone), "app.rs still has `{gone}`");
    }
}
