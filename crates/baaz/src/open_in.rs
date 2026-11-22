//! "Open in editor" (docs/82 7.5) as the window uses it: the chosen editor
//! from Settings › General › "Open files in" ([`crate::layout`]'s `openIn`),
//! the words a control wears for it, and the one call every control makes
//! ([`open_path`]). The model — detection and the argv — is
//! [`baaz_core::editors`].
//!
//! Three controls open through here: the file preview header's open button
//! (at the 7.1 selection's first line when lines are held), a Changes row's
//! open button (at its first hunk when the diff is loaded), and the session
//! ⋯ menu's "Open project in …" (the session's folder: its worktree, else
//! the project root).

use std::path::{Path, PathBuf};

use baaz_core::editors::{self, DetectedEditor};

use crate::layout::LayoutGlobal as _;

/// The editors this run offers. A capture lists none, so a probe's
/// screens never depend on what the machine running it has installed.
pub(crate) fn detected() -> &'static [DetectedEditor] {
    if crate::runtime::captures() {
        return &[];
    }
    editors::detected()
}

/// The editor files open in now: `None` is macOS's default app (nothing
/// chosen, or the chosen one is gone).
pub(crate) fn chosen(cx: &gpui::App) -> Option<&'static DetectedEditor> {
    editors::chosen(crate::layout::Layout::get(cx).open_in.as_deref(), detected())
}

/// "Open in Zed" / "Open in default app": the file controls' label.
pub(crate) fn open_label(cx: &gpui::App) -> String {
    editors::open_label(chosen(cx))
}

/// The ⋯ menu's row, only when an editor is chosen: "Open project in Zed".
/// With the default app a folder would only open in Finder, which is not an
/// editor (rule 13: a control is shown only if it does what it says).
pub(crate) fn project_label(cx: &gpui::App) -> Option<String> {
    chosen(cx).map(|found| format!("Open project in {}", found.editor.name))
}

/// The setting's choice word: the editor's name, or "Default app".
pub(crate) fn choice_label(cx: &gpui::App) -> String {
    chosen(cx).map_or_else(|| "Default app".to_owned(), |found| found.editor.name.to_owned())
}

/// Open `path` (at 1-based `line` when the editor takes one) in the chosen
/// editor, off the UI thread. Tests record the argv instead of running it.
pub(crate) fn open_path(path: &Path, line: Option<u32>, cx: &gpui::App) {
    let argv = editors::open_command(chosen(cx), path, line, &|p: &Path| p.exists(), &editors::which);
    spawn(argv);
}

#[cfg(not(all(test, feature = "probe")))]
fn spawn(argv: Vec<String>) {
    editors::spawn_detached(argv, |why| crate::baaz_log!("open in editor: could not start {why}"));
}

#[cfg(all(test, feature = "probe"))]
fn spawn(argv: Vec<String>) {
    OPENED.with(|opened| opened.borrow_mut().push(argv));
}

#[cfg(all(test, feature = "probe"))]
thread_local! {
    /// The argv each [`open_path`] would have run, for tests.
    pub(crate) static OPENED: std::cell::RefCell<Vec<Vec<String>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Select `folder` in Finder (a session row's Reveal folder, docs/82 8.5),
/// off the UI thread: the existence check and `open -R` run on a thread of
/// their own. A missing folder does nothing. Tests record the path
/// instead of opening Finder.
pub(crate) fn reveal_folder(folder: PathBuf) {
    reveal(folder);
}

#[cfg(not(all(test, feature = "probe")))]
fn reveal(folder: PathBuf) {
    let _ = std::thread::Builder::new().name("baaz-reveal".into()).spawn(move || {
        if std::fs::metadata(&folder).is_ok() {
            if let Err(why) = crate::system_open::open_detached([std::ffi::OsStr::new("-R"), folder.as_os_str()]) {
                crate::baaz_log!("reveal folder: could not start open ({why})");
            }
        }
    });
}

#[cfg(all(test, feature = "probe"))]
fn reveal(folder: PathBuf) {
    REVEALED.with(|revealed| revealed.borrow_mut().push(folder));
}

#[cfg(all(test, feature = "probe"))]
thread_local! {
    /// The folders [`reveal_folder`] would have shown, for tests.
    pub(crate) static REVEALED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The folder a session's "Open project in …" opens: its workspace path
/// (a worktree session's own worktree). `None` for an empty path.
pub(crate) fn project_folder(workspace: &str) -> Option<PathBuf> {
    let trimmed = workspace.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;

    #[gpui::test]
    fn the_default_opens_with_plain_open_and_the_labels_say_so(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(open_label(cx), "Open in default app");
            assert_eq!(choice_label(cx), "Default app");
            assert!(project_label(cx).is_none(), "no editor: no project row");
            OPENED.with(|o| o.borrow_mut().clear());
            open_path(Path::new("/w/a.rs"), Some(3), cx);
            OPENED.with(|o| assert_eq!(o.borrow().as_slice(), [vec!["/usr/bin/open".to_owned(), "/w/a.rs".to_owned()]]));
        });
        assert_eq!(project_folder("  "), None);
        assert_eq!(project_folder("/w/tree"), Some(PathBuf::from("/w/tree")));
    }
}
