//! Removes Chromium's code-sign clones left by Baaz runs that died (docs/74 §1).
//!
//! Chromium clones the running app to
//! `<X>/<bundle id>.code_sign_clone/code_sign_clone.XXXXXX/Baaz.app.bundle`
//! (about 400 MB listed) a few seconds after it starts, and removes the clone
//! from a helper it launches during shutdown. A run that never reaches that
//! shutdown (SIGKILL, a crash, a deleted bundle) leaves its clone until the
//! Mac reboots: upstream has only a TODO for this (crbug.com/343784575).
//!
//! **Why this can never remove a live instance's clone.** Chromium hard-links
//! the main executable into the clone, so a clone's `Contents/MacOS/<exe>` *is*
//! the inode its owner is executing, even if the bundle has since been swapped
//! or deleted. The sweep therefore lists the text inodes of every process
//! first, and removes a clone only if no other process executes one of its
//! inodes. An instance creates its clone seconds after it execs, so the owner of
//! any clone that already existed when the list was taken is in that list. If
//! the list cannot be trusted (`lsof` failed, or it does not show this very
//! process), nothing is removed.
//!
//! Two instances of one build share that inode, so while another instance of
//! the same build runs, stale clones of that build stay, and the next sweep gets
//! them. That is the safe direction. Instances that sweep at the same moment
//! only race to remove the same dead directory, and a lost race is ignored.
//!
//! Reach is limited by construction: exact directory names from [`OWN_IDS`],
//! no pattern over `*.code_sign_clone`, and only children named the way
//! Chromium names them. Chrome's and every other app's clones sit in the same
//! parent and are never listed.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Every bundle ID Baaz's own builds run under. Exact names, on purpose.
pub const OWN_IDS: &[&str] = &["sh.baaz.app", "sh.baaz.check", "sh.baaz.d6check", "sh.baaz.m3check", "sh.baaz.leaktest"];

/// How old a clone must be before a launch's sweep looks at it. The inode test
/// is the guarantee; this only keeps a sweep away from a clone mid-creation.
pub const STARTUP_GRACE: Duration = Duration::from_secs(60);

/// The prefix and random-suffix length of Chromium's per-run directory.
const RUN_PREFIX: &str = "code_sign_clone.";
const RUN_SUFFIX_LEN: usize = 6;

/// What one sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub removed: usize,
    pub in_use: usize,
    pub too_young: usize,
    /// Nothing was removed because the process list could not be trusted.
    pub aborted: bool,
}

/// `<X>`, the per-user directory Chromium's clones live under, from the system
/// rather than `$TMPDIR` (scripted launches run with a bare environment).
pub fn clone_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = vec![0u8; 1024];
        // SAFETY: `buf` is writable for the length passed.
        let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
        if len == 0 || len > buf.len() {
            return None;
        }
        let temp = PathBuf::from(String::from_utf8_lossy(&buf[..len - 1]).into_owned());
        return Some(temp.canonicalize().ok()?.parent()?.join("X"));
    }
    #[allow(unreachable_code)]
    None
}

/// The per-run directories under `root` for `ids`, and nothing else.
fn candidates(root: &Path, ids: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for id in ids {
        let Ok(entries) = std::fs::read_dir(root.join(format!("{id}.code_sign_clone"))) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(RUN_PREFIX)) else { continue };
            // `file_type` does not follow links: a symlink is never ours.
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if suffix.len() == RUN_SUFFIX_LEN && is_dir {
                out.push(entry.path());
            }
        }
    }
    out
}

/// The inodes in `<run>/*/Contents/MacOS`, where the hard link to the owner's
/// executable lives.
fn executable_inodes(run: &Path) -> HashSet<u64> {
    let mut out = HashSet::new();
    let Ok(bundles) = std::fs::read_dir(run) else { return out };
    for bundle in bundles.flatten() {
        let Ok(files) = std::fs::read_dir(bundle.path().join("Contents/MacOS")) else { continue };
        for file in files.flatten() {
            if let Ok(meta) = std::fs::symlink_metadata(file.path()) {
                out.insert(meta.ino());
            }
        }
    }
    out
}

/// The sweep itself, over a process list taken at `listed_at`.
///
/// `running` is the text inodes of every process except this one. A clone newer
/// than `listed_at - grace` is left alone: its owner may not be in the list.
pub fn sweep(root: &Path, ids: &[&str], running: &HashSet<u64>, listed_at: SystemTime, grace: Duration) -> Report {
    let mut report = Report::default();
    let cutoff = listed_at.checked_sub(grace).unwrap_or(listed_at);
    for run in candidates(root, ids) {
        let created = std::fs::symlink_metadata(&run).and_then(|meta| meta.modified());
        // No readable age means it cannot be proved old.
        if !created.map(|at| at < cutoff).unwrap_or(false) {
            report.too_young += 1;
            continue;
        }
        if executable_inodes(&run).iter().any(|inode| running.contains(inode)) {
            report.in_use += 1;
            continue;
        }
        // Another instance may have got here first: gone is the same result.
        match std::fs::remove_dir_all(&run) {
            Ok(()) => report.removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => crate::baaz_log!("clone sweep: could not remove {}: {error}", run.display()),
        }
    }
    report
}

/// Parses `lsof -d txt -Fpi` into the text inodes of every process but `me`.
/// `None` when the output does not include `me`: then the list proves nothing.
fn parse_lsof(output: &str, me: u32) -> Option<HashSet<u64>> {
    let mut inodes = HashSet::new();
    let (mut pid, mut saw_me) = (0u32, false);
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix('i') {
            let Ok(inode) = value.parse::<u64>() else { continue };
            if pid == me {
                saw_me = true;
            } else {
                inodes.insert(inode);
            }
        }
    }
    saw_me.then_some(inodes)
}

/// The text inodes of every other process, from `/usr/sbin/lsof`.
fn running_inodes() -> Option<HashSet<u64>> {
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-w", "-d", "txt", "-Fpi"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    parse_lsof(&String::from_utf8_lossy(&output.stdout), std::process::id())
}

/// One sweep of this user's own clone directories. Blocking: `lsof` can take
/// seconds, so it runs only when there is something to judge.
pub fn sweep_own(grace: Duration) -> Report {
    let Some(root) = clone_root() else { return Report::default() };
    if candidates(&root, OWN_IDS).is_empty() {
        return Report::default();
    }
    // Taken before the list, so every clone it may remove predates the list.
    let listed_at = SystemTime::now();
    let Some(running) = running_inodes() else {
        crate::baaz_log!("clone sweep: the process list could not be trusted, removed nothing");
        return Report { aborted: true, ..Report::default() };
    };
    sweep(&root, OWN_IDS, &running, listed_at, grace)
}

/// The launch's sweep, off the UI thread. Failure is only ever a log line.
#[cfg_attr(not(feature = "cef"), allow(dead_code))]
pub fn sweep_in_background() {
    let spawned = std::thread::Builder::new().name("clone-sweep".into()).spawn(|| {
        let report = sweep_own(STARTUP_GRACE);
        if report != Report::default() {
            crate::baaz_log!(
                "clone sweep: removed {}, in use {}, too young {}",
                report.removed,
                report.in_use,
                report.too_young
            );
        }
    });
    if let Err(error) = spawned {
        crate::baaz_log!("clone sweep: no thread: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG_AGO: Duration = Duration::from_secs(3600);

    struct Root(PathBuf);
    impl Root {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("baaz-sweep-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        /// A clone as Chromium lays it out; returns its executable's inode.
        fn clone_at(&self, id: &str, suffix: &str) -> u64 {
            let macos = self
                .0
                .join(format!("{id}.code_sign_clone/{RUN_PREFIX}{suffix}/Baaz.app.bundle/Contents/MacOS"));
            std::fs::create_dir_all(&macos).unwrap();
            let exe = macos.join("Baaz");
            std::fs::write(&exe, b"x").unwrap();
            std::fs::metadata(&exe).unwrap().ino()
        }
        fn run(&self, id: &str, suffix: &str) -> PathBuf {
            self.0.join(format!("{id}.code_sign_clone/{RUN_PREFIX}{suffix}"))
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn later() -> SystemTime {
        SystemTime::now() + LONG_AGO
    }

    #[test]
    fn a_dead_clone_goes_and_a_running_ones_stays() {
        let root = Root::new("live");
        root.clone_at("sh.baaz.app", "dead01");
        let live = root.clone_at("sh.baaz.app", "live01");
        let running = HashSet::from([live]);
        let report = sweep(&root.0, &["sh.baaz.app"], &running, later(), Duration::ZERO);
        assert_eq!(report, Report { removed: 1, in_use: 1, ..Report::default() });
        assert!(!root.run("sh.baaz.app", "dead01").exists());
        assert!(root.run("sh.baaz.app", "live01").exists());
    }

    #[test]
    fn a_clone_newer_than_the_grace_stays_even_with_no_owner_listed() {
        let root = Root::new("young");
        root.clone_at("sh.baaz.app", "young1");
        let report = sweep(&root.0, &["sh.baaz.app"], &HashSet::new(), SystemTime::now(), STARTUP_GRACE);
        assert_eq!(report, Report { too_young: 1, ..Report::default() });
        assert!(root.run("sh.baaz.app", "young1").exists());
    }

    #[test]
    fn other_apps_and_unexpected_names_are_out_of_reach() {
        let root = Root::new("reach");
        root.clone_at("com.google.Chrome", "chrome");
        root.clone_at("sh.baaz.appx", "lookal");
        root.clone_at("sh.baaz.app", "toolongname");
        let stray = root.0.join("sh.baaz.app.code_sign_clone/notes");
        std::fs::create_dir_all(&stray).unwrap();
        // A symlink named like a run must not be followed out of the directory.
        let outside = root.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let link = root.0.join(format!("sh.baaz.app.code_sign_clone/{RUN_PREFIX}linked"));
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let report = sweep(&root.0, OWN_IDS, &HashSet::new(), later(), Duration::ZERO);
        assert_eq!(report, Report::default());
        assert!(root.run("com.google.Chrome", "chrome").exists());
        assert!(root.run("sh.baaz.appx", "lookal").exists());
        assert!(root.run("sh.baaz.app", "toolongname").exists());
        assert!(stray.exists() && outside.exists() && link.exists());
    }

    #[test]
    fn the_allowlist_names_no_pattern_and_no_other_app() {
        for id in OWN_IDS {
            assert!(id.starts_with("sh.baaz."), "{id}");
            assert!(!id.contains('*'), "{id}");
        }
    }

    #[test]
    fn two_sweeps_at_once_do_not_fail_on_each_other() {
        let root = Root::new("race");
        for suffix in ["aaaaa1", "aaaaa2", "aaaaa3", "aaaaa4"] {
            root.clone_at("sh.baaz.check", suffix);
        }
        let (path, at) = (root.0.clone(), later());
        let other = std::thread::spawn({
            let path = path.clone();
            move || sweep(&path, &["sh.baaz.check"], &HashSet::new(), at, Duration::ZERO)
        });
        let mine = sweep(&path, &["sh.baaz.check"], &HashSet::new(), at, Duration::ZERO);
        let theirs = other.join().unwrap();
        assert!(mine.removed + theirs.removed >= 4);
        assert!(candidates(&path, &["sh.baaz.check"]).is_empty());
    }

    #[test]
    fn lsof_output_excludes_me_and_requires_me() {
        let output = "p10\nftxt\ni111\nftxt\ni222\np20\nftxt\ni333\n";
        assert_eq!(parse_lsof(output, 20), Some(HashSet::from([111, 222])));
        assert_eq!(parse_lsof(output, 30), None, "a list without this process proves nothing");
        assert_eq!(parse_lsof("", 20), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_process_list_is_trusted_on_this_machine() {
        let running = running_inodes().expect("lsof must list this process");
        assert!(!running.is_empty());
        let root = clone_root().expect("the X directory");
        assert_eq!(root.file_name().and_then(|name| name.to_str()), Some("X"));
    }
}
