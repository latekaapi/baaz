//! "Open in editor" (docs/82 7.5): which editors this Mac has, and the one
//! command that opens a path — optionally at a line — in the chosen one.
//!
//! Detection never runs an editor: it looks for the app bundle in
//! `/Applications` and `~/Applications` ([`detect_in`] takes the folders, so
//! a test hands it a fake one). Opening is `open -a <bundle> <path>`
//! ([`open_command`]), except that an editor with a line argument goes
//! through its own command-line tool when that tool is inside the bundle or
//! on `PATH` — Zed's `cli path:line`, VS Code / Cursor / Windsurf's
//! `--goto path:line`, Sublime's `subl path:line`, Xcode's `xed --line`,
//! JetBrains' `--line`. Without one the editor still opens the file, at its
//! top. The choice "Default app" ([`DEFAULT_APP`]) is plain `open <path>`.
//!
//! Nothing here spawns: [`open_command`] returns the argv and
//! [`spawn_detached`] runs it (`Command::spawn`, `posix_spawn` underneath —
//! never `pre_exec`, docs/82 rule 5), off the caller's thread.

use std::path::{Path, PathBuf};

/// The stored choice for macOS's own pick: `open <path>`.
pub const DEFAULT_APP: &str = "default";

/// How an editor takes a line on its command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineArg {
    /// `<tool> path:line` (Zed, Sublime Text).
    Suffix,
    /// `<tool> --goto path:line` (VS Code and its forks).
    Goto,
    /// `<tool> --line N path` (Xcode's `xed`, JetBrains launchers).
    LineFlag,
    /// `<tool> -l N path` (TextMate's `mate`).
    DashL,
    /// `<tool> +N path` (BBEdit).
    Plus,
}

/// One editor Baaz knows how to find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownEditor {
    /// The stored id (`layout.json`'s `openIn`).
    pub id: &'static str,
    /// The name the controls show ("Open in Zed").
    pub name: &'static str,
    /// The bundle's file name in an Applications folder.
    pub bundle: &'static str,
    /// The command-line tool inside the bundle, relative to it, if the
    /// bundle ships one.
    pub bundled_cli: Option<&'static str>,
    /// The tool's name on `PATH`, if it installs one there.
    pub path_cli: Option<&'static str>,
    /// How the tool takes a line; `None` opens at the top.
    pub line: Option<LineArg>,
}

/// Every editor detection looks for, in the order the setting lists them.
pub const KNOWN_EDITORS: &[KnownEditor] = &[
    KnownEditor { id: "zed", name: "Zed", bundle: "Zed.app", bundled_cli: Some("Contents/MacOS/cli"), path_cli: Some("zed"), line: Some(LineArg::Suffix) },
    KnownEditor { id: "zed-preview", name: "Zed Preview", bundle: "Zed Preview.app", bundled_cli: Some("Contents/MacOS/cli"), path_cli: None, line: Some(LineArg::Suffix) },
    KnownEditor { id: "vscode", name: "VS Code", bundle: "Visual Studio Code.app", bundled_cli: Some("Contents/Resources/app/bin/code"), path_cli: Some("code"), line: Some(LineArg::Goto) },
    KnownEditor { id: "cursor", name: "Cursor", bundle: "Cursor.app", bundled_cli: Some("Contents/Resources/app/bin/cursor"), path_cli: Some("cursor"), line: Some(LineArg::Goto) },
    KnownEditor { id: "windsurf", name: "Windsurf", bundle: "Windsurf.app", bundled_cli: Some("Contents/Resources/app/bin/windsurf"), path_cli: Some("windsurf"), line: Some(LineArg::Goto) },
    KnownEditor { id: "sublime", name: "Sublime Text", bundle: "Sublime Text.app", bundled_cli: Some("Contents/SharedSupport/bin/subl"), path_cli: Some("subl"), line: Some(LineArg::Suffix) },
    KnownEditor { id: "nova", name: "Nova", bundle: "Nova.app", bundled_cli: None, path_cli: None, line: None },
    KnownEditor { id: "bbedit", name: "BBEdit", bundle: "BBEdit.app", bundled_cli: None, path_cli: Some("bbedit"), line: Some(LineArg::Plus) },
    KnownEditor { id: "textmate", name: "TextMate", bundle: "TextMate.app", bundled_cli: None, path_cli: Some("mate"), line: Some(LineArg::DashL) },
    KnownEditor { id: "xcode", name: "Xcode", bundle: "Xcode.app", bundled_cli: Some("Contents/Developer/usr/bin/xed"), path_cli: Some("xed"), line: Some(LineArg::LineFlag) },
    KnownEditor { id: "rustrover", name: "RustRover", bundle: "RustRover.app", bundled_cli: Some("Contents/MacOS/rustrover"), path_cli: Some("rustrover"), line: Some(LineArg::LineFlag) },
    KnownEditor { id: "intellij", name: "IntelliJ IDEA", bundle: "IntelliJ IDEA.app", bundled_cli: Some("Contents/MacOS/idea"), path_cli: Some("idea"), line: Some(LineArg::LineFlag) },
    KnownEditor { id: "intellij-ce", name: "IntelliJ IDEA CE", bundle: "IntelliJ IDEA CE.app", bundled_cli: Some("Contents/MacOS/idea"), path_cli: None, line: Some(LineArg::LineFlag) },
    KnownEditor { id: "webstorm", name: "WebStorm", bundle: "WebStorm.app", bundled_cli: Some("Contents/MacOS/webstorm"), path_cli: Some("webstorm"), line: Some(LineArg::LineFlag) },
    KnownEditor { id: "pycharm", name: "PyCharm", bundle: "PyCharm.app", bundled_cli: Some("Contents/MacOS/pycharm"), path_cli: Some("pycharm"), line: Some(LineArg::LineFlag) },
    KnownEditor { id: "goland", name: "GoLand", bundle: "GoLand.app", bundled_cli: Some("Contents/MacOS/goland"), path_cli: Some("goland"), line: Some(LineArg::LineFlag) },
    KnownEditor { id: "clion", name: "CLion", bundle: "CLion.app", bundled_cli: Some("Contents/MacOS/clion"), path_cli: Some("clion"), line: Some(LineArg::LineFlag) },
];

/// An editor found on this Mac: what it is and where its bundle lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedEditor {
    /// Which known editor.
    pub editor: KnownEditor,
    /// The bundle found (`/Applications/Zed.app`).
    pub app: PathBuf,
}

/// The Applications folders detection looks in: `/Applications`, then the
/// person's own `~/Applications` (where JetBrains Toolbox installs).
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications"));
    }
    dirs
}

/// The known editors whose bundle exists in one of `dirs`, in
/// [`KNOWN_EDITORS`] order; the first folder holding a bundle wins. Only
/// checks that the bundle is a directory — nothing is launched or read.
pub fn detect_in(dirs: &[PathBuf]) -> Vec<DetectedEditor> {
    KNOWN_EDITORS
        .iter()
        .filter_map(|editor| {
            dirs.iter()
                .map(|dir| dir.join(editor.bundle))
                .find(|app| app.is_dir())
                .map(|app| DetectedEditor { editor: *editor, app })
        })
        .collect()
}

/// This Mac's editors, looked up once per run (the bundles do not move
/// under a running app often enough to re-walk on every menu).
pub fn detected() -> &'static [DetectedEditor] {
    static DETECTED: std::sync::OnceLock<Vec<DetectedEditor>> = std::sync::OnceLock::new();
    DETECTED.get_or_init(|| detect_in(&application_dirs()))
}

/// What the stored `choice` opens with among `detected`: `None` for
/// [`DEFAULT_APP`], an unknown id, or an editor no longer installed (the
/// default then stands, so a removed editor never leaves a dead control).
pub fn chosen<'a>(choice: Option<&str>, detected: &'a [DetectedEditor]) -> Option<&'a DetectedEditor> {
    let id = choice?;
    if id == DEFAULT_APP {
        return None;
    }
    detected.iter().find(|found| found.editor.id == id)
}

/// The control's words: "Open in Zed", or "Open in default app".
pub fn open_label(editor: Option<&DetectedEditor>) -> String {
    match editor {
        Some(found) => format!("Open in {}", found.editor.name),
        None => "Open in default app".to_owned(),
    }
}

/// The command-line tool `editor` takes a line through: the bundled one if
/// it exists, else its `PATH` name if `on_path` finds it.
pub fn line_tool(editor: &DetectedEditor, exists: &dyn Fn(&Path) -> bool, on_path: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    editor.editor.line?;
    if let Some(rel) = editor.editor.bundled_cli {
        let tool = editor.app.join(rel);
        if exists(&tool) {
            return Some(tool);
        }
    }
    editor.editor.path_cli.and_then(on_path)
}

/// The argv that opens `path` (at `line`, 1-based, when given and the
/// editor can) with `editor`, or with macOS's own pick when `editor` is
/// `None`. `exists` and `on_path` are the filesystem seams: production
/// passes [`Path::exists`] and [`which`], tests pass fakes.
pub fn open_command(
    editor: Option<&DetectedEditor>,
    path: &Path,
    line: Option<u32>,
    exists: &dyn Fn(&Path) -> bool,
    on_path: &dyn Fn(&str) -> Option<PathBuf>,
) -> Vec<String> {
    let open = "/usr/bin/open".to_owned();
    let path_text = path.to_string_lossy().into_owned();
    let Some(editor) = editor else {
        return vec![open, path_text];
    };
    let at_line = line.filter(|n| *n > 0).zip(editor.editor.line);
    if let Some((n, style)) = at_line {
        if let Some(tool) = line_tool(editor, exists, on_path) {
            let tool = tool.to_string_lossy().into_owned();
            return match style {
                LineArg::Suffix => vec![tool, format!("{path_text}:{n}")],
                LineArg::Goto => vec![tool, "--goto".to_owned(), format!("{path_text}:{n}")],
                LineArg::LineFlag => vec![tool, "--line".to_owned(), n.to_string(), path_text],
                LineArg::DashL => vec![tool, "-l".to_owned(), n.to_string(), path_text],
                LineArg::Plus => vec![tool, format!("+{n}"), path_text],
            };
        }
    }
    vec![open, "-a".to_owned(), editor.app.to_string_lossy().into_owned(), path_text]
}

/// `name` on `PATH`, as a shell would find it: the first directory holding
/// an entry of that name.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

/// Run `argv` detached on a background thread and reap it there: the UI
/// thread neither forks nor waits. `posix_spawn` underneath (no
/// `pre_exec`). A failure to start is logged by the caller's `on_error`.
pub fn spawn_detached(argv: Vec<String>, on_error: impl FnOnce(String) + Send + 'static) {
    // No thread means nothing ran; there is no one to tell but the caller's
    // log, and the thread's own error path covers a failed start.
    let _ = std::thread::Builder::new().name("baaz-open-editor".into()).spawn(move || {
        let Some((program, args)) = argv.split_first() else { return };
        match std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let _ = child.wait();
            }
            Err(error) => on_error(format!("{program}: {error}")),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway Applications folder holding `names` as bundle folders;
    /// removed on drop.
    struct FakeApps(PathBuf);

    impl FakeApps {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for FakeApps {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fake_apps(names: &[&str]) -> FakeApps {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("baaz-editors-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in names {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        FakeApps(dir)
    }

    #[test]
    fn detection_finds_bundles_in_a_fake_applications_folder_in_known_order() {
        let dir = fake_apps(&["Visual Studio Code.app", "Zed.app", "Notes.app", "RustRover.app"]);
        // A plain file named like a bundle is not an app.
        std::fs::write(dir.path().join("Cursor.app"), b"").unwrap();
        let found = detect_in(&[dir.path().to_path_buf()]);
        let ids: Vec<&str> = found.iter().map(|f| f.editor.id).collect();
        assert_eq!(ids, ["zed", "vscode", "rustrover"]);
        assert_eq!(found[0].app, dir.path().join("Zed.app"));
        assert!(detect_in(&[dir.path().join("missing")]).is_empty());
    }

    #[test]
    fn the_first_folder_holding_a_bundle_wins() {
        let system = fake_apps(&["Zed.app"]);
        let own = fake_apps(&["Zed.app", "Sublime Text.app"]);
        let found = detect_in(&[system.path().to_path_buf(), own.path().to_path_buf()]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].app, system.path().join("Zed.app"));
        assert_eq!(found[1].app, own.path().join("Sublime Text.app"));
    }

    #[test]
    fn chosen_falls_back_to_the_default_for_unknown_or_missing_editors() {
        let dir = fake_apps(&["Zed.app"]);
        let found = detect_in(&[dir.path().to_path_buf()]);
        assert_eq!(chosen(Some("zed"), &found).map(|f| f.editor.id), Some("zed"));
        assert!(chosen(None, &found).is_none());
        assert!(chosen(Some(DEFAULT_APP), &found).is_none());
        assert!(chosen(Some("cursor"), &found).is_none(), "an uninstalled pick opens with the default");
        assert_eq!(open_label(chosen(Some("zed"), &found)), "Open in Zed");
        assert_eq!(open_label(None), "Open in default app");
    }

    fn editor(id: &str) -> DetectedEditor {
        let editor = *KNOWN_EDITORS.iter().find(|e| e.id == id).unwrap();
        DetectedEditor { editor, app: PathBuf::from(format!("/Applications/{}", editor.bundle)) }
    }

    #[test]
    fn launch_command_per_editor_with_and_without_a_line() {
        let file = Path::new("/w/src/main.rs");
        let everything = |_: &Path| true;
        let nothing_on_path = |_: &str| None;
        // Default app: plain `open`, the line dropped.
        assert_eq!(open_command(None, file, Some(4), &everything, &nothing_on_path), ["/usr/bin/open", "/w/src/main.rs"]);
        // No line: every editor opens through `open -a`.
        for id in ["zed", "vscode", "cursor", "sublime", "xcode", "nova", "rustrover"] {
            let e = editor(id);
            assert_eq!(
                open_command(Some(&e), file, None, &everything, &nothing_on_path),
                vec!["/usr/bin/open".to_owned(), "-a".to_owned(), e.app.to_string_lossy().into_owned(), "/w/src/main.rs".to_owned()],
                "{id}"
            );
        }
        // With a line, through the bundled tool.
        assert_eq!(open_command(Some(&editor("zed")), file, Some(12), &everything, &nothing_on_path), ["/Applications/Zed.app/Contents/MacOS/cli", "/w/src/main.rs:12"]);
        assert_eq!(
            open_command(Some(&editor("vscode")), file, Some(12), &everything, &nothing_on_path),
            ["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code", "--goto", "/w/src/main.rs:12"]
        );
        assert_eq!(
            open_command(Some(&editor("cursor")), file, Some(3), &everything, &nothing_on_path),
            ["/Applications/Cursor.app/Contents/Resources/app/bin/cursor", "--goto", "/w/src/main.rs:3"]
        );
        assert_eq!(
            open_command(Some(&editor("windsurf")), file, Some(3), &everything, &nothing_on_path),
            ["/Applications/Windsurf.app/Contents/Resources/app/bin/windsurf", "--goto", "/w/src/main.rs:3"]
        );
        assert_eq!(
            open_command(Some(&editor("xcode")), file, Some(7), &everything, &nothing_on_path),
            ["/Applications/Xcode.app/Contents/Developer/usr/bin/xed", "--line", "7", "/w/src/main.rs"]
        );
        // Nova takes no line: `open -a` even when one is asked for.
        assert_eq!(open_command(Some(&editor("nova")), file, Some(7), &everything, &nothing_on_path)[1], "-a");
        // Line 0 is no line.
        assert_eq!(open_command(Some(&editor("zed")), file, Some(0), &everything, &nothing_on_path)[1], "-a");
    }

    #[test]
    fn a_line_needs_the_tool_in_the_bundle_or_on_path_else_open_a() {
        let file = Path::new("/w/a.txt");
        let none = |_: &Path| false;
        let nothing_on_path = |_: &str| None;
        let on_path = |name: &str| Some(PathBuf::from(format!("/usr/local/bin/{name}")));
        // No bundled tool and nothing on PATH: open -a, at the top.
        let zed = editor("zed");
        assert_eq!(open_command(Some(&zed), file, Some(5), &none, &nothing_on_path)[..2], ["/usr/bin/open", "-a"]);
        // The PATH tool stands in for a missing bundled one.
        assert_eq!(open_command(Some(&zed), file, Some(5), &none, &on_path), ["/usr/local/bin/zed", "/w/a.txt:5"]);
        // BBEdit and TextMate only ever come from PATH.
        assert_eq!(open_command(Some(&editor("bbedit")), file, Some(5), &none, &on_path), ["/usr/local/bin/bbedit", "+5", "/w/a.txt"]);
        assert_eq!(open_command(Some(&editor("textmate")), file, Some(5), &none, &on_path), ["/usr/local/bin/mate", "-l", "5", "/w/a.txt"]);
        // Zed Preview has no PATH tool: without its bundled cli it is open -a.
        assert_eq!(open_command(Some(&editor("zed-preview")), file, Some(5), &none, &on_path)[1], "-a");
    }

    #[test]
    fn known_editor_ids_are_unique_and_never_the_default() {
        let mut ids: Vec<&str> = KNOWN_EDITORS.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), KNOWN_EDITORS.len());
        assert!(!ids.contains(&DEFAULT_APP));
    }
}
