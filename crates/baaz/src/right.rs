//! The right pane's four bodies: real read-only data, inert labelled actions.
//!
//! [`render`] dispatches on [`RightKind`](crate::layout::RightKind) to one
//! builder per pane. Reads are synchronous and cheap (a one-level directory
//! walk, a handful of read-only `git` invocations each with a timeout); every
//! action button is inert by owner decision and says so through [`inert`].

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use aui::workbench::{
    browser_nav, diff_review, file_tree, git_changes, pr_form, BrowserAction, DiffReviewAction, DiffScope,
    DiffView, FileNode, FileTreeAction, GitAction, PrAction, PrDescription, ReviewFile,
};
use aui_icons::FileType;
use aui_protocol::{ChangeKind, Diff, DiffKind, DiffLine, FileChange, Hunk};
use gpui::{div, prelude::*, px, AnyElement, App, Context, ElementId, SharedString, WeakEntity, Window};
use gpui_kit::base::v_flex;

use crate::app::Harness;
use crate::layout::RightKind;

/// How many file-tree rows the Files pane will hold. A directory with 10,000
/// entries must not be read into the pane; the footer names the cap.
pub(crate) const FILE_WALK_CAP: usize = 300;
/// How many files of a working-tree diff reach the Diff pane.
pub(crate) const DIFF_FILES_CAP: usize = 16;
/// How many body lines of one file reach the Diff pane.
pub(crate) const DIFF_LINES_PER_FILE_CAP: usize = 500;
/// One read-only git invocation may take this long before it counts as hung.
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
/// Directory names never walked into, so a naive walk stays small.
const SKIP_DIRS: [&str; 3] = [".git", "target", "node_modules"];

/// Render the right pane for `kind`.
///
/// The signature stays `(kind, cx)`: this function never reads the `Harness`
/// entity itself — `render` runs while `Harness` is borrowed for update, so
/// `cx.entity().read(cx)` would panic. Instead the project comes from the
/// projects store on disk (the same source `Harness` settles from at boot,
/// resolved with the same [`crate::projects::Projects::effective_current`]
/// rule), and toasts travel through a weak handle the action closures
/// upgrade at click time, when no borrow is held.
pub(crate) fn render(kind: RightKind, cx: &mut Context<Harness>) -> AnyElement {
    let notify = toast_sink(cx.weak_entity());
    let project = crate::projects::read()
        .effective_current()
        .map(|current| (current.root.clone(), current.name.clone()));
    match kind {
        RightKind::Browser => browser_pane(&notify),
        RightKind::Diff => match project {
            Some((root, _)) => diff_pane(&root, &notify),
            None => empty_state(
                "right-diff-empty",
                "Diff review",
                "No project is open",
                "Open or adopt a project and its unstaged changes will be reviewed here.",
            ),
        },
        RightKind::Git => match project {
            Some((root, _)) => git_pane(&root, &notify),
            None => empty_state(
                "right-git-empty",
                "Changes",
                "No project is open",
                "Open or adopt a project and its uncommitted changes will be listed here.",
            ),
        },
        RightKind::Files => match project {
            Some((root, name)) => files_pane(&root, &name, &notify),
            None => empty_state(
                "right-files-empty",
                "Files",
                "No project is open",
                "Open or adopt a project to browse its files here.",
            ),
        },
    }
}

/// A labelled placeholder: never a blank pane, always a role and a label.
fn empty_state(id: &'static str, pane: &'static str, heading: &'static str, detail: &'static str) -> AnyElement {
    v_flex()
        .id(id)
        .role(gpui::Role::Group)
        .aria_label(pane)
        .size_full()
        .p(px(16.0))
        .gap(px(8.0))
        .child(div().child(heading))
        .child(
            div()
                .id((ElementId::from(id), "detail"))
                .role(gpui::Role::Label)
                .aria_label(format!("{pane} detail"))
                .child(detail),
        )
        .into_any_element()
}

// ── the inert-action path ────────────────────────────────────────────────

/// The exact stderr line [`inert`] emits, split out so tests can assert it
/// without capturing stderr.
fn inert_text(pane: &'static str, action: &str) -> String {
    format!("right pane: {pane} action {action} is not wired yet")
}

/// Record of every [`inert`] call, test builds only: the app's stderr log
/// cannot be asserted from a test, so the message is also pushed here.
#[cfg(test)]
fn inert_log_store() -> &'static std::sync::Mutex<Vec<String>> {
    static LOG: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// What [`inert`] recorded so far, oldest first.
#[cfg(test)]
pub(crate) fn inert_log() -> Vec<String> {
    inert_log_store().lock().map(|log| log.clone()).unwrap_or_default()
}

/// Forget what [`inert`] recorded.
#[cfg(test)]
pub(crate) fn clear_inert_log() {
    if let Ok(mut log) = inert_log_store().lock() {
        log.clear();
    }
}

/// How an inert action raises its toast: the title and body, plus the app to
/// raise it in. A closure over [`toast_sink`] in production, a recording stub
/// in tests.
type ToastSink = Rc<dyn Fn(String, String, &mut App)>;

/// Build the production [`ToastSink`]: upgrade the weak `Harness` at click
/// time — when no update borrow is held — and toast through its overlays.
/// Nothing happens when the window is already gone, apart from the log line.
fn toast_sink(weak: WeakEntity<Harness>) -> ToastSink {
    Rc::new(move |title, body, cx| {
        if let Some(harness) = weak.upgrade() {
            harness.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| overlays.toast(title, body));
            });
        }
    })
}

/// The one shared inert handler: log the attempt on stderr and raise the
/// app's existing toast naming the action. Returns the log line so tests can
/// see it. Never shells out, never mutates: Commit and Push land here too.
fn inert(pane: &'static str, action: &str, notify: &ToastSink, cx: &mut App) -> String {
    let line = inert_text(pane, action);
    crate::baaz_log!("{line}");
    #[cfg(test)]
    if let Ok(mut log) = inert_log_store().lock() {
        log.push(line.clone());
    }
    notify(
        "Not wired yet".to_string(),
        format!("{action} in the {pane} pane is not wired yet, so nothing happened."),
        cx,
    );
    line
}

/// Exhaustive action names: no `_` arm, so the compiler tells the next
/// person when the library adds a variant.
fn files_action_name(action: &FileTreeAction) -> String {
    match action {
        FileTreeAction::Select(id) => format!("Select {id}"),
        FileTreeAction::Toggle(id) => format!("Toggle {id}"),
        FileTreeAction::Search => "Search".to_string(),
        FileTreeAction::Refresh => "Refresh".to_string(),
    }
}

fn browser_action_name(action: &BrowserAction) -> String {
    match action {
        BrowserAction::Back => "Back".to_string(),
        BrowserAction::Forward => "Forward".to_string(),
        BrowserAction::Reload => "Reload".to_string(),
        BrowserAction::FocusUrl => "FocusUrl".to_string(),
        BrowserAction::ToggleAnnotate => "ToggleAnnotate".to_string(),
        BrowserAction::Screenshot => "Screenshot".to_string(),
        BrowserAction::Console => "Console".to_string(),
    }
}

fn git_action_name(action: &GitAction) -> String {
    match action {
        GitAction::Toggle(path) => format!("Toggle {path}"),
        GitAction::Regenerate => "Regenerate".to_string(),
        GitAction::Amend => "Amend".to_string(),
        GitAction::Commit => "Commit".to_string(),
        GitAction::Push => "Push".to_string(),
    }
}

fn pr_action_name(action: &PrAction) -> String {
    match action {
        PrAction::PickBase => "PickBase".to_string(),
        PrAction::Cancel => "Cancel".to_string(),
        PrAction::Create => "Create".to_string(),
        PrAction::Linear => "Linear".to_string(),
    }
}

fn diff_action_name(action: &DiffReviewAction) -> String {
    match action {
        DiffReviewAction::Scope(scope) => format!("Scope {}", scope.label()),
        DiffReviewAction::View(view) => format!("View {}", view.label()),
        DiffReviewAction::Search => "Search".to_string(),
        DiffReviewAction::CollapseAll => "CollapseAll".to_string(),
        DiffReviewAction::SelectFile(path) => format!("SelectFile {path}"),
        DiffReviewAction::OpenInEditor => "OpenInEditor".to_string(),
        DiffReviewAction::Stage => "Stage".to_string(),
        DiffReviewAction::AddNote(line) => format!("AddNote {line}"),
        DiffReviewAction::EditNote(index) => format!("EditNote {index}"),
        DiffReviewAction::DeleteNote(index) => format!("DeleteNote {index}"),
        DiffReviewAction::SaveNote(index) => format!("SaveNote {index}"),
        DiffReviewAction::CancelNote(index) => format!("CancelNote {index}"),
        DiffReviewAction::Clear => "Clear".to_string(),
        DiffReviewAction::Send => "Send".to_string(),
    }
}

/// One handler per component, each a thin call to [`inert`]. Factored as
/// named functions (rather than inline closures) so tests can invoke the
/// exact handler the pane wires up.
fn files_handler(notify: ToastSink) -> impl Fn(&FileTreeAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Files", &files_action_name(action), &notify, cx);
    }
}

fn browser_handler(notify: ToastSink) -> impl Fn(BrowserAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Browser", &browser_action_name(&action), &notify, cx);
    }
}

fn git_handler(notify: ToastSink) -> impl Fn(GitAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Changes", &git_action_name(&action), &notify, cx);
    }
}

fn pr_handler(notify: ToastSink) -> impl Fn(PrAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Pull request", &pr_action_name(&action), &notify, cx);
    }
}

fn diff_handler(notify: ToastSink) -> impl Fn(DiffReviewAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Diff review", &diff_action_name(&action), &notify, cx);
    }
}

// ── read-only git ────────────────────────────────────────────────────────

/// Run one read-only git command in `root`, giving up after [`GIT_TIMEOUT`].
/// `None` covers everything that can go wrong: no git binary, not a repo,
/// a hung subprocess. The pane degrades to an empty state instead.
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let root = root.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        let _ = tx.send(output);
    });
    match rx.recv_timeout(GIT_TIMEOUT) {
        Ok(Ok(output)) if output.status.success() => String::from_utf8(output.stdout).ok(),
        _ => None,
    }
}

/// Strip one layer of C-style quoting from a git pathname.
fn unquote(path: &str) -> &str {
    let path = path.trim();
    if path.len() >= 2 && path.starts_with('"') && path.ends_with('"') {
        &path[1..path.len() - 1]
    } else {
        path
    }
}

/// One porcelain v1 line into `(path, staged, kind)`. `staged` is whether the
/// index column is non-space. Renames report the new path. `None` for lines
/// with nothing to show (ignored files, blank lines, malformed rows).
fn parse_porcelain_line(line: &str) -> Option<(String, bool, ChangeKind)> {
    let bytes = line.as_bytes();
    if bytes.len() < 4 || bytes[2] != b' ' {
        return None;
    }
    let index = bytes[0] as char;
    let worktree = bytes[1] as char;
    if index == '!' {
        return None;
    }
    let raw = line[3..].split(" -> ").last().unwrap_or(&line[3..]);
    let path = unquote(raw);
    if path.is_empty() {
        return None;
    }
    let staged = index != ' ' && index != '?';
    let kind = if index == 'A' || worktree == 'A' || index == '?' || worktree == '?' {
        ChangeKind::Added
    } else if index == 'D' || worktree == 'D' {
        ChangeKind::Deleted
    } else {
        ChangeKind::Modified
    };
    Some((path.to_string(), staged, kind))
}

/// One `--numstat` output into path → (added, removed). Binary files report
/// `- -` and count as no line changes.
fn parse_numstat(text: &str) -> HashMap<String, (u32, u32)> {
    let mut counts = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(added), Some(removed), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let path = path.split(" => ").last().unwrap_or(path);
        let path = unquote(path).to_string();
        if path.is_empty() {
            continue;
        }
        let pair = match (added.parse::<u32>(), removed.parse::<u32>()) {
            (Ok(added), Ok(removed)) => (added, removed),
            _ => (0, 0),
        };
        counts.insert(path, pair);
    }
    counts
}

/// Parse `rev-list --count --left-right upstream...HEAD`: the left count is
/// behind, the right count is ahead. `None` when there is no upstream, which
/// the caller falls back to `(0, 0)`.
fn parse_ahead_behind(text: &str) -> Option<(u32, u32)> {
    let mut fields = text.split_whitespace();
    let behind: u32 = fields.next()?.parse().ok()?;
    let ahead: u32 = fields.next()?.parse().ok()?;
    Some((ahead, behind))
}

/// Everything the Git pane reads, or `None` when the project is not a git
/// repo at all.
struct GitStatus {
    files: Vec<(FileChange, bool)>,
    branch: String,
    ahead: u32,
    behind: u32,
    added: u32,
    removed: u32,
}

fn read_git_status(root: &Path) -> Option<GitStatus> {
    let porcelain = run_git(root, &["status", "--porcelain=v1", "--untracked-files=normal"])?;
    let branch = run_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let (ahead, behind) = run_git(root, &["rev-list", "--count", "--left-right", "@{upstream}...HEAD"])
        .and_then(|out| parse_ahead_behind(&out))
        .unwrap_or((0, 0));
    let unstaged = run_git(root, &["diff", "--numstat"]).map(|out| parse_numstat(&out)).unwrap_or_default();
    let staged = run_git(root, &["diff", "--cached", "--numstat"])
        .map(|out| parse_numstat(&out))
        .unwrap_or_default();
    let mut files = Vec::new();
    let mut added = 0_u32;
    let mut removed = 0_u32;
    for line in porcelain.lines() {
        let Some((path, is_staged, kind)) = parse_porcelain_line(line) else {
            continue;
        };
        let counts = if is_staged {
            staged.get(&path).or_else(|| unstaged.get(&path))
        } else {
            unstaged.get(&path)
        };
        let (file_added, file_removed) = counts.copied().unwrap_or((0, 0));
        added += file_added;
        removed += file_removed;
        files.push((
            FileChange { path, change: kind, added: file_added, removed: file_removed },
            is_staged,
        ));
    }
    files.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    Some(GitStatus { files, branch, ahead, behind, added, removed })
}

// ── the file walk ────────────────────────────────────────────────────────

/// The row icon for a file name, by extension.
fn file_type_for(name: &str) -> FileType {
    let lower = name.to_ascii_lowercase();
    if lower.contains(".test.") || lower.contains(".spec.") {
        FileType::Test
    } else if lower.ends_with(".tsx") {
        FileType::Tsx
    } else if lower.ends_with(".ts") {
        FileType::Ts
    } else if lower.ends_with(".json") || lower.ends_with(".lock") {
        if lower.ends_with(".lock") { FileType::Lock } else { FileType::Json }
    } else if lower.ends_with(".md") {
        FileType::Md
    } else if lower.ends_with(".css") {
        FileType::Css
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".svg")
        || lower.ends_with(".webp")
        || lower.ends_with(".ico")
    {
        FileType::Image
    } else {
        FileType::File
    }
}

/// One level of `root`, sorted by name, skipping [`SKIP_DIRS`]. Directories
/// arrive closed: expansion state lives in `Harness`, which this task may not
/// touch, so expansion is not functional (see the report). Returns the nodes
/// and whether the [`FILE_WALK_CAP`] cut the listing short.
fn walk_root_capped(root: &Path, cap: usize) -> (Vec<FileNode>, bool) {
    let mut dir = match std::fs::read_dir(root) {
        Ok(dir) => dir.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(_) => return (Vec::new(), false),
    };
    dir.sort_by_key(|entry| entry.file_name());
    let mut nodes = Vec::new();
    for entry in dir {
        if nodes.len() >= cap {
            return (nodes, true);
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().as_ref().is_ok_and(|kind| kind.is_dir());
        // By name, whatever it is on disk. In a git WORKTREE `.git` is a
        // file holding a `gitdir:` pointer, not a directory, so an
        // `is_dir &&` guard here would list it — which is exactly what it
        // did until a capture showed `.git` sitting at the top of the tree.
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        if is_dir {
            nodes.push(FileNode::dir(name.clone(), name, false).depth(0));
        } else {
            let kind = file_type_for(&name);
            nodes.push(FileNode::file(name.clone(), name, kind).depth(0));
        }
    }
    (nodes, false)
}

// ── the unified-diff parser ──────────────────────────────────────────────

/// What [`parse_unified_diff`] kept, and whether the caps cut anything.
struct ParsedDiffs {
    diffs: Vec<Diff>,
    truncated: bool,
}

/// Parse `@@ -old[,len] +new[,len] @@` into its two start lines.
fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
    let mut fields = header.split_whitespace();
    if fields.next()? != "@@" {
        return None;
    }
    let old = fields.next()?.strip_prefix('-')?;
    let new = fields.next()?.strip_prefix('+')?;
    let old_start: u32 = old.split(',').next()?.parse().ok()?;
    let new_start: u32 = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// Strip the `a/` or `b/` prefix git puts on diff paths.
fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("b/").or_else(|| path.strip_prefix("a/")).unwrap_or(path)
}

/// Parse a working-tree `git diff` into per-file [`Diff`]s, capped at
/// [`DIFF_FILES_CAP`] files and [`DIFF_LINES_PER_FILE_CAP`] body lines each.
/// Defensive throughout: binary diffs, renames, new empty files and missing
/// trailing newlines all parse without panicking.
fn parse_unified_diff(text: &str) -> ParsedDiffs {
    let mut diffs: Vec<Diff> = Vec::new();
    let mut truncated = false;
    let mut path: Option<String> = None;
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut added = 0_u32;
    let mut removed = 0_u32;
    let mut kept_lines = 0_usize;
    let mut old_no = 1_u32;
    let mut new_no = 1_u32;

    // The per-file M/A/D kind is re-derived from porcelain, not from the
    // diff: `new file mode` / `deleted file mode` lines carry no rows, and
    // the `--- /dev/null` / `+++ /dev/null` markers below only disambiguate
    // paths, so neither is tracked here.
    let finish_file = |path: &mut Option<String>,
                           hunks: &mut Vec<Hunk>,
                           added: &mut u32,
                           removed: &mut u32,
                           kept_lines: &mut usize,
                           diffs: &mut Vec<Diff>| {
        if let Some(path) = path.take() {
            diffs.push(Diff { path, hunks: std::mem::take(hunks), added: *added, removed: *removed });
        }
        *added = 0;
        *removed = 0;
        *kept_lines = 0;
    };

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish_file(&mut path, &mut hunks, &mut added, &mut removed, &mut kept_lines, &mut diffs);
            if diffs.len() >= DIFF_FILES_CAP {
                truncated = true;
                break;
            }
            let mut sides = rest.split(' ');
            let old_side = sides.next().unwrap_or("");
            let new_side = sides.next().unwrap_or("");
            let next = if new_side == "/dev/null" { old_side } else { new_side };
            let next = strip_diff_prefix(next);
            if !next.is_empty() {
                path = Some(next.to_string());
            }
        } else if line.starts_with("new file mode") || line.starts_with("deleted file mode") {
            // Kind only; the rows below carry the content.
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            path = Some(strip_diff_prefix(rest.trim()).to_string());
        } else if line.starts_with("Binary files ") {
            // Nothing follows for this file: zero hunks, zero counts.
        } else if line.starts_with("--- ") {
            // `/dev/null` here means a new file; the path came from `diff --git`.
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let side = rest.trim();
            if side != "/dev/null" && path.is_none() {
                path = Some(strip_diff_prefix(side).to_string());
            }
        } else if line.starts_with("@@") {
            if path.is_none() {
                continue;
            }
            let (old_start, new_start) = parse_hunk_header(line).unwrap_or((1, 1));
            old_no = old_start;
            new_no = new_start;
            hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
        } else if line.starts_with('\\') {
            // `\ No newline at end of file`: a marker, not a row.
        } else if path.is_some() && !hunks.is_empty() {
            let (kind, text) = if let Some(rest) = line.strip_prefix('+') {
                (DiffKind::Add, rest)
            } else if let Some(rest) = line.strip_prefix('-') {
                (DiffKind::Del, rest)
            } else if let Some(rest) = line.strip_prefix(' ') {
                (DiffKind::Context, rest)
            } else if line.is_empty() {
                // A context row whose trailing space did not survive.
                (DiffKind::Context, line)
            } else {
                // `index …`, `similarity …`, mode lines: not rows.
                continue;
            };
            if kept_lines >= DIFF_LINES_PER_FILE_CAP {
                truncated = true;
                continue;
            }
            kept_lines += 1;
            let (old, new) = match kind {
                DiffKind::Context => {
                    let pair = (Some(old_no), Some(new_no));
                    old_no += 1;
                    new_no += 1;
                    pair
                }
                DiffKind::Add => {
                    added += 1;
                    let pair = (None, Some(new_no));
                    new_no += 1;
                    pair
                }
                DiffKind::Del => {
                    removed += 1;
                    let pair = (Some(old_no), None);
                    old_no += 1;
                    pair
                }
            };
            if let Some(hunk) = hunks.last_mut() {
                hunk.lines.push(DiffLine { kind, old_no: old, new_no: new, text: text.to_string() });
            }
        }
    }
    finish_file(&mut path, &mut hunks, &mut added, &mut removed, &mut kept_lines, &mut diffs);
    ParsedDiffs { diffs, truncated }
}

// ── the four panes ───────────────────────────────────────────────────

/// The file tree, real: one level of the project root. Header names the
/// project's directory; the footer counts the nodes and names the cap.
fn files_pane(root: &Path, name: &str, notify: &ToastSink) -> AnyElement {
    let (nodes, truncated) = walk_root_capped(root, FILE_WALK_CAP);
    let count = nodes.len();
    let footer = if truncated {
        format!("{count} shown — capped at the first {FILE_WALK_CAP} entries")
    } else if count == 1 {
        "1 file or folder".to_string()
    } else {
        format!("{count} files and folders")
    };
    let tree = file_tree("right-files", nodes)
        .header(name)
        .footer(footer)
        .on_action(files_handler(notify.clone()));
    v_flex()
        .id("right-files-pane")
        .role(gpui::Role::Group)
        .aria_label("Files")
        .size_full()
        .child(tree)
        .into_any_element()
}

/// Chrome only: the nav row over a surface that says plainly that no web
/// engine is attached yet.
fn browser_pane(notify: &ToastSink) -> AnyElement {
    let nav = browser_nav("right-browser-nav", "about:blank")
        .secure(true)
        .can_go_back(false)
        .can_go_forward(false)
        .on_action(browser_handler(notify.clone()));
    v_flex()
        .id("right-browser-pane")
        .role(gpui::Role::Group)
        .aria_label("Browser")
        .size_full()
        .child(nav)
        .child(
            div()
                .id("right-browser-placeholder")
                .role(gpui::Role::Label)
                .aria_label("Browser placeholder")
                .p(px(16.0))
                .child(
                    "There is no web engine attached yet, so this pane cannot show a page. \
                     The address bar above is chrome only and does nothing for now.",
                ),
        )
        .into_any_element()
}

/// Real status, inert verbs: the changes panel plus the PR form. The form
/// has no head parameter, so the real branch rides in the description; the
/// checks stay empty rather than fabricating green rows. Commit and Push
/// reach [`inert`] and never shell out.
fn git_pane(root: &Path, notify: &ToastSink) -> AnyElement {
    let Some(status) = read_git_status(root) else {
        return empty_state(
            "right-git-empty",
            "Changes",
            "Not a git repository",
            "This project's folder is not a git checkout, so there are no changes to show.",
        );
    };
    let changes = git_changes("right-git", status.files, "", status.ahead, status.behind)
        .branch(status.branch.clone())
        .on_action(git_handler(notify.clone()));
    let form = pr_form(
        "right-pr",
        "main",
        "",
        vec![PrDescription::Text(SharedString::from(format!("Head branch: {}", status.branch)))],
        Vec::new(),
    )
    .on_action(pr_handler(notify.clone()));
    v_flex()
        .id("right-git-pane")
        .role(gpui::Role::Group)
        .aria_label("Changes")
        .size_full()
        .child(changes)
        .child(form)
        .into_any_element()
}

/// The working tree, read-only: unstaged diff parsed with caps, the first
/// file selected, real numstat totals in the summary.
fn diff_pane(root: &Path, notify: &ToastSink) -> AnyElement {
    let Some(status) = read_git_status(root) else {
        return empty_state(
            "right-diff-empty",
            "Diff review",
            "Not a git repository",
            "This project's folder is not a git checkout, so there is nothing to review.",
        );
    };
    if status.files.is_empty() {
        return empty_state(
            "right-diff-clean",
            "Diff review",
            "No unstaged changes",
            "The working tree is clean — there is nothing to review.",
        );
    }
    let raw = run_git(root, &["diff", "--no-color", "--no-ext-diff", "--unified=3"]).unwrap_or_default();
    let parsed = parse_unified_diff(&raw);
    let review_files: Vec<ReviewFile> = status.files.iter().take(DIFF_FILES_CAP).enumerate()
        .map(|(index, (change, _))| ReviewFile { change: change.clone(), notes: 0, selected: index == 0 })
        .collect();
    let first_path = review_files.first().map(|file| file.change.path.clone()).unwrap_or_default();
    let shown = parsed
        .diffs
        .iter()
        .find(|diff| diff.path == first_path)
        .or_else(|| parsed.diffs.first())
        .cloned()
        .unwrap_or(Diff { path: first_path, hunks: Vec::new(), added: 0, removed: 0 });
    let lead = if parsed.truncated {
        format!(
            "Unstaged changes — showing the first {DIFF_FILES_CAP} files, \
             {DIFF_LINES_PER_FILE_CAP} lines each"
        )
    } else {
        "Unstaged changes".to_string()
    };
    let review = diff_review("right-diff", review_files, shown, Vec::new(), DiffScope::Unstaged, DiffView::Unified)
        .summary(lead, status.added, status.removed)
        .on_action(diff_handler(notify.clone()));
    v_flex()
        .id("right-diff-pane")
        .role(gpui::Role::Group)
        .aria_label("Diff review")
        .size_full()
        .child(review)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("baaz-right-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // ── the diff parser's four defensive shapes ──

    #[test]
    fn a_binary_file_diff_parses_without_rows() {
        let text = "diff --git a/img.png b/img.png\nnew file mode 100644\nindex 0000000..e69de29\nBinary files /dev/null and b/img.png differ\n";
        let parsed = parse_unified_diff(text);
        assert!(!parsed.truncated);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].path, "img.png");
        assert!(parsed.diffs[0].hunks.is_empty());
        assert_eq!((parsed.diffs[0].added, parsed.diffs[0].removed), (0, 0));
    }

    #[test]
    fn a_rename_parses_under_its_new_path() {
        let text = "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\nindex 1234567..89abcde 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";
        let parsed = parse_unified_diff(text);
        assert!(!parsed.truncated);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].path, "new.rs");
        assert_eq!(parsed.diffs[0].hunks.len(), 1);
        assert_eq!((parsed.diffs[0].added, parsed.diffs[0].removed), (1, 1));
        let lines = &parsed.diffs[0].hunks[0].lines;
        assert_eq!(lines.len(), 3);
        assert_eq!((lines[0].kind, lines[0].old_no, lines[0].new_no), (DiffKind::Context, Some(1), Some(1)));
        assert_eq!((lines[1].kind, lines[1].old_no, lines[1].new_no), (DiffKind::Del, Some(2), None));
        assert_eq!(lines[1].text, "old");
        assert_eq!((lines[2].kind, lines[2].old_no, lines[2].new_no), (DiffKind::Add, None, Some(2)));
        assert_eq!(lines[2].text, "new");
    }

    #[test]
    fn a_new_empty_file_parses_without_hunks() {
        let text =
            "diff --git a/empty.txt b/empty.txt\nnew file mode 100644\nindex 0000000..e69de29\n";
        let parsed = parse_unified_diff(text);
        assert!(!parsed.truncated);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].path, "empty.txt");
        assert!(parsed.diffs[0].hunks.is_empty());
    }

    #[test]
    fn a_missing_trailing_newline_leaves_no_marker_row() {
        let text = "diff --git a/a.txt b/a.txt\nindex 1234567..89abcde 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let parsed = parse_unified_diff(text);
        assert_eq!(parsed.diffs.len(), 1);
        let lines = &parsed.diffs[0].hunks[0].lines;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "old");
        assert_eq!(lines[1].text, "new");
        assert_eq!((parsed.diffs[0].added, parsed.diffs[0].removed), (1, 1));
    }

    #[test]
    fn the_diff_caps_files_and_lines_per_file() {
        let mut text = String::new();
        for index in 0..(DIFF_FILES_CAP + 4) {
            text.push_str(&format!("diff --git a/f{index}.txt b/f{index}.txt\n--- a/f{index}.txt\n+++ b/f{index}.txt\n@@ -1 +1 @@\n-x\n+y\n"));
        }
        let parsed = parse_unified_diff(&text);
        assert_eq!(parsed.diffs.len(), DIFF_FILES_CAP);
        assert!(parsed.truncated);

        let mut one = String::from("diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1 +1 @@\n");
        for _ in 0..(DIFF_LINES_PER_FILE_CAP + 50) {
            one.push_str("+y\n");
        }
        let parsed = parse_unified_diff(&one);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].hunks[0].lines.len(), DIFF_LINES_PER_FILE_CAP);
        assert!(parsed.truncated);
    }

    // ── porcelain, numstat and the fallbacks ──

    #[test]
    fn porcelain_letters_map_to_kinds_and_staged_flags() {
        let (path, staged, kind) = parse_porcelain_line("M  src/a.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("src/a.rs", true, ChangeKind::Modified));
        let (path, staged, kind) = parse_porcelain_line(" M src/b.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("src/b.rs", false, ChangeKind::Modified));
        let (path, staged, kind) = parse_porcelain_line("A  new.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("new.rs", true, ChangeKind::Added));
        let (path, staged, kind) = parse_porcelain_line("?? untracked.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("untracked.rs", false, ChangeKind::Added));
        let (path, staged, kind) = parse_porcelain_line(" D gone.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("gone.rs", false, ChangeKind::Deleted));
        let (path, staged, kind) = parse_porcelain_line("R  old.rs -> new.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("new.rs", true, ChangeKind::Modified));
        assert!(parse_porcelain_line("").is_none());
        assert!(parse_porcelain_line("!! ignored.rs").is_none());
    }

    #[test]
    fn numstat_counts_binary_files_as_zero() {
        let counts = parse_numstat("10\t2\ta.rs\n-\t-\timg.png\n");
        assert_eq!(counts.get("a.rs"), Some(&(10, 2)));
        assert_eq!(counts.get("img.png"), Some(&(0, 0)));
    }

    #[test]
    fn the_upstream_fallback_is_zero_and_zero() {
        assert_eq!(parse_ahead_behind("3\t7\n"), Some((7, 3)));
        assert_eq!(parse_ahead_behind(""), None);
        assert_eq!(parse_ahead_behind("not a rev-list output"), None);
        // The pane's own fallback when the upstream read fails.
        let (ahead, behind) = parse_ahead_behind("").unwrap_or((0, 0));
        assert_eq!((ahead, behind), (0, 0));
    }

    #[test]
    fn a_non_repo_degrades_to_no_status() {
        let root = temp_root("non-repo");
        assert!(run_git(&root, &["status", "--porcelain"]).is_none());
        assert!(read_git_status(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── the file walk ──

    #[test]
    fn the_walk_lists_one_level_and_skips_vendored_dirs() {
        let root = temp_root("walk");
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("inner.txt"), "inner").unwrap();
        for skipped in SKIP_DIRS {
            std::fs::create_dir_all(root.join(skipped)).unwrap();
            std::fs::write(root.join(skipped).join("x"), "x").unwrap();
        }
        let (nodes, truncated) = walk_root_capped(&root, FILE_WALK_CAP);
        assert!(!truncated);
        let names: Vec<String> = nodes.iter().map(|node| node.name.to_string()).collect();
        assert_eq!(names, vec!["a.txt".to_string(), "sub".to_string()]);
        assert!(nodes.iter().all(|node| node.depth == 0));
        assert!(nodes[1].is_dir());
        assert!(!nodes[0].is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A git worktree's `.git` is a FILE, not a directory. The skip must be
    /// by name alone: the directory-shaped test above passes either way, so
    /// only this one fails if the `is_dir &&` guard comes back.
    #[test]
    fn the_walk_skips_a_file_shaped_dot_git() {
        let root = temp_root("walk-worktree");
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        let (nodes, _) = walk_root_capped(&root, FILE_WALK_CAP);
        let names: Vec<String> = nodes.iter().map(|node| node.name.to_string()).collect();
        assert_eq!(names, vec!["a.txt".to_string()], "a file-shaped .git must be skipped too");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_walk_caps_huge_directories() {
        let root = temp_root("walk-cap");
        for index in 0..5 {
            std::fs::write(root.join(format!("f{index}.txt")), "x").unwrap();
        }
        let (nodes, truncated) = walk_root_capped(&root, 3);
        assert_eq!(nodes.len(), 3);
        assert!(truncated);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_icons_follow_extensions() {
        assert_eq!(file_type_for("a.ts"), FileType::Ts);
        assert_eq!(file_type_for("a.tsx"), FileType::Tsx);
        assert_eq!(file_type_for("a.json"), FileType::Json);
        assert_eq!(file_type_for("a.md"), FileType::Md);
        assert_eq!(file_type_for("Cargo.lock"), FileType::Lock);
        assert_eq!(file_type_for("a.test.ts"), FileType::Test);
        assert_eq!(file_type_for("a.png"), FileType::Image);
        assert_eq!(file_type_for("main.rs"), FileType::File);
    }

    // ── the inert handlers, invoked directly ──

    /// A [`ToastSink`] that records titles and bodies instead of toasting.
    fn test_sink(toasts: Rc<std::cell::RefCell<Vec<(String, String)>>>) -> ToastSink {
        Rc::new(move |title, body, _| toasts.borrow_mut().push((title, body)))
    }

    #[gpui::test]
    fn files_actions_all_reach_inert(cx: &mut TestAppContext) {
        let vc = cx.add_empty_window();
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let handle = files_handler(test_sink(toasts.clone()));
            handle(&FileTreeAction::Select(SharedString::from("src/main.rs")), window, cx);
            handle(&FileTreeAction::Toggle(SharedString::from("src")), window, cx);
            handle(&FileTreeAction::Search, window, cx);
            handle(&FileTreeAction::Refresh, window, cx);
        });
        let log = inert_log();
        assert_eq!(log.len(), 4);
        assert!(log[0].contains("Files") && log[0].contains("Select src/main.rs"), "unexpected: {}", log[0]);
        assert!(log[1].contains("Toggle src"), "unexpected: {}", log[1]);
        assert!(log[2].contains("Search"), "unexpected: {}", log[2]);
        assert!(log[3].contains("Refresh"), "unexpected: {}", log[3]);
        let toasts = toasts.borrow();
        assert_eq!(toasts.len(), 4);
        assert!(toasts.iter().all(|(title, body)| title == "Not wired yet" && body.contains("Files")));
        assert!(toasts[0].1.contains("Select src/main.rs"));
    }

    #[gpui::test]
    fn browser_actions_all_reach_inert(cx: &mut TestAppContext) {
        let vc = cx.add_empty_window();
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let handle = browser_handler(test_sink(toasts.clone()));
            for action in [
                BrowserAction::Back,
                BrowserAction::Forward,
                BrowserAction::Reload,
                BrowserAction::FocusUrl,
                BrowserAction::ToggleAnnotate,
                BrowserAction::Screenshot,
                BrowserAction::Console,
            ] {
                handle(action, window, cx);
            }
        });
        let log = inert_log();
        assert_eq!(log.len(), 7);
        for line in &log {
            assert!(line.contains("Browser"), "unexpected: {line}");
        }
        assert!(log.iter().any(|line| line.contains("Back")));
        assert!(log.iter().any(|line| line.contains("Screenshot")));
        assert_eq!(toasts.borrow().len(), 7);
    }

    #[gpui::test]
    fn git_and_pr_actions_all_reach_inert(cx: &mut TestAppContext) {
        let vc = cx.add_empty_window();
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let git = git_handler(test_sink(toasts.clone()));
            git(GitAction::Toggle(SharedString::from("a.rs")), window, cx);
            git(GitAction::Regenerate, window, cx);
            git(GitAction::Amend, window, cx);
            git(GitAction::Commit, window, cx);
            git(GitAction::Push, window, cx);
            let pr = pr_handler(test_sink(toasts.clone()));
            pr(PrAction::PickBase, window, cx);
            pr(PrAction::Cancel, window, cx);
            pr(PrAction::Create, window, cx);
            pr(PrAction::Linear, window, cx);
        });
        let log = inert_log();
        assert_eq!(log.len(), 9);
        assert!(log.iter().any(|line| line.contains("Changes") && line.contains("Commit")));
        assert!(log.iter().any(|line| line.contains("Changes") && line.contains("Push")));
        assert!(log.iter().any(|line| line.contains("Pull request") && line.contains("Create")));
        assert_eq!(toasts.borrow().len(), 9);
    }

    #[gpui::test]
    fn diff_actions_all_reach_inert(cx: &mut TestAppContext) {
        let vc = cx.add_empty_window();
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let handle = diff_handler(test_sink(toasts.clone()));
            for action in [
                DiffReviewAction::Scope(DiffScope::Unstaged),
                DiffReviewAction::View(DiffView::Unified),
                DiffReviewAction::Search,
                DiffReviewAction::CollapseAll,
                DiffReviewAction::SelectFile(SharedString::from("a.rs")),
                DiffReviewAction::OpenInEditor,
                DiffReviewAction::Stage,
                DiffReviewAction::AddNote(3),
                DiffReviewAction::EditNote(0),
                DiffReviewAction::DeleteNote(0),
                DiffReviewAction::SaveNote(0),
                DiffReviewAction::CancelNote(0),
                DiffReviewAction::Clear,
                DiffReviewAction::Send,
            ] {
                handle(action, window, cx);
            }
        });
        let log = inert_log();
        assert_eq!(log.len(), 14);
        for line in &log {
            assert!(line.contains("Diff review"), "unexpected: {line}");
        }
        assert!(log.iter().any(|line| line.contains("Stage")));
        assert!(log.iter().any(|line| line.contains("Send")));
        assert_eq!(toasts.borrow().len(), 14);
    }
}

