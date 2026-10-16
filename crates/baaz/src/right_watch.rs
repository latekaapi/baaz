//! A file watcher on the right pane's project root, so Changes and Files
//! refresh when the disk changes instead of on a 2 s poll paced by render
//! (perf audit 2026-10-16: an idle window went stale, a busy one forked six
//! git commands every 2 s).
//!
//! FSEvents through `notify`, recursive. Paths that change constantly and
//! never change what the pane shows — build output, dependency folders,
//! git's object store — are dropped before they wake anything.

use std::path::{Path, PathBuf};

use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

/// A live watch on one root. Dropping it stops the watch.
pub(crate) struct RightWatch {
    root: PathBuf,
    _watcher: RecommendedWatcher,
}

impl RightWatch {
    /// Watch `root`; every change that matters sends one `()` on the
    /// returned channel. `None` when the platform refuses the watch.
    pub(crate) fn start(root: &Path) -> Option<(Self, UnboundedReceiver<()>)> {
        let (tx, rx) = unbounded();
        let watched = root.to_path_buf();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            if event.paths.iter().any(|path| matters(&watched, path)) {
                let _ = tx.unbounded_send(());
            }
        })
        .ok()?;
        watcher.watch(root, RecursiveMode::Recursive).ok()?;
        Some((Self { root: root.to_path_buf(), _watcher: watcher }, rx))
    }

    /// The root this watch follows.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// Folders whose churn never changes what Changes or Files show.
const NOISY: &[&str] = &["target", "node_modules", ".next", "dist", "build", ".venv", "__pycache__"];

/// Whether a change at `path` under `root` can change the pane: anything in
/// the worktree outside [`NOISY`] folders, and in `.git` only what moves the
/// status (the index, HEAD, refs).
pub(crate) fn matters(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else { return true };
    let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy());
    match parts.next().as_deref() {
        None => true,
        Some(".git") => matches!(parts.next().as_deref(), Some("index" | "HEAD" | "refs" | "packed-refs")),
        Some(first) if NOISY.contains(&first) => false,
        Some(_) => !rel.components().any(|c| NOISY.contains(&c.as_os_str().to_string_lossy().as_ref())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changes_the_pane_can_show_matter() {
        let root = Path::new("/work/repo");
        assert!(matters(root, Path::new("/work/repo/src/lib.rs")));
        assert!(matters(root, Path::new("/work/repo/.git/index")));
        assert!(matters(root, Path::new("/work/repo/.git/refs/heads/main")));
        assert!(!matters(root, Path::new("/work/repo/.git/objects/ab/cdef")));
        assert!(!matters(root, Path::new("/work/repo/target/debug/baaz")));
        assert!(!matters(root, Path::new("/work/repo/web/node_modules/x/index.js")));
        assert!(matters(root, Path::new("/elsewhere/file")), "an unknown path errs toward a refresh");
    }

    #[test]
    fn a_write_under_the_root_rings_the_watch() {
        let dir = std::env::temp_dir().join(format!("baaz-right-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let dir = dir.canonicalize().expect("canonical");
        let (_watch, mut rx) = RightWatch::start(&dir).expect("watch starts");
        // FSEvents needs a moment to arm before the first write is seen.
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(dir.join("seen.txt"), b"x").expect("write");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut rang = false;
        while std::time::Instant::now() < deadline {
            if rx.try_recv().is_ok() {
                rang = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(rang, "a write in the watched root wakes the pane");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
