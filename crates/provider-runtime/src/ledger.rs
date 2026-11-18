//! The pid ledger: every child a Baaz spawned, swept at the next launch.
//!
//! A Baaz that dies without hanging up (SIGKILL, a crash, the quit budget
//! running out) leaves its children running — a Full-access turn going on
//! unsupervised (docs/81 B PM-2). Each [`ChildProcess`](crate::ChildProcess)
//! is recorded in `<state>/run/children.json` while it lives, with the
//! identity of the Baaz that owns it; the next launch [`sweep`]s the file.
//!
//! **Only what can be proven ours is killed.** An entry is swept only when
//! its owner is gone (no process has the owner's pid with the owner's start
//! time — a second Baaz on the same directory keeps its children), and its
//! child is signalled only when a process with the child's pid *and* the
//! child's start time is alive. A reused pid has a different start time and
//! is never touched. The group signal that follows reaches the child's own
//! group: a group id is its leader's pid, and a pid cannot be reused while
//! the leader lives or while its group has members.
//!
//! What a sweep cannot prove: a group whose leader already exited (a
//! grandchild left behind by a child that died with Baaz). Nothing then ties
//! the group id to this ledger, so it is left alone.
//!
//! The file is read and rewritten under an exclusive `flock` on a sibling
//! `.lock` file and replaced atomically, so two processes recording at once
//! never lose an entry.

use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};

use crate::process::{end_group, Identity};

/// The ledger's file name inside the state directory's `run/`.
pub const FILE_NAME: &str = "children.json";

/// The format written; a file with another version is read as empty.
const VERSION: u64 = 1;

static INSTALLED: OnceLock<Ledger> = OnceLock::new();

/// One child, recorded while it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The child (its pid is also its group id).
    pub child: Identity,
    /// The program it runs, for the log.
    pub program: String,
    /// The Baaz that spawned it.
    pub owner: Identity,
}

/// A ledger file and the process recording into it.
pub struct Ledger {
    path: PathBuf,
    owner: Identity,
}

/// The process-wide ledger, once [`install`]ed. Every
/// [`ChildProcess`](crate::ChildProcess) records into it.
pub fn installed() -> Option<&'static Ledger> {
    INSTALLED.get()
}

/// Record every child this process spawns from now on in `path`, owned by
/// this process. The first install wins; `None` when this process's own
/// identity cannot be read (nothing is then recorded).
pub fn install(path: PathBuf) -> Option<&'static Ledger> {
    let owner = Identity::current()?;
    Some(INSTALLED.get_or_init(|| Ledger::new(path, owner)))
}

/// Launch-time wiring: [`install`] `dir/children.json` for this process,
/// then [`sweep`] what dead owners left there on a thread of its own, and
/// hand the outcome to `report` (for the log).
pub fn launch(dir: &Path, report: impl FnOnce(std::io::Result<Sweep>) + Send + 'static) {
    let path = dir.join(FILE_NAME);
    install(path.clone());
    let spawned = std::thread::Builder::new()
        .name("pid-ledger-sweep".into())
        .spawn(move || report(sweep(&path, crate::TERM_GRACE)));
    if let Err(error) = spawned {
        eprintln!("baaz: no thread for the pid ledger sweep: {error}");
    }
}

impl Ledger {
    /// A ledger at `path` recording for `owner`.
    pub fn new(path: PathBuf, owner: Identity) -> Self {
        Self { path, owner }
    }

    /// Where the ledger lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record a running child.
    pub fn record(&self, child: Identity, program: &str) -> std::io::Result<()> {
        let entry = Entry { child, program: program.to_owned(), owner: self.owner };
        locked(&self.path, |entries| {
            entries.retain(|existing| existing.child != child);
            entries.push(entry);
            true
        })
    }

    /// Forget a child that has been reaped.
    pub fn forget(&self, child: Identity) -> std::io::Result<()> {
        locked(&self.path, |entries| {
            let before = entries.len();
            entries.retain(|existing| existing.child != child);
            entries.len() != before
        })
    }
}

/// What a [`sweep`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Children of dead owners that were still running: their groups were
    /// ended (SIGTERM, then SIGKILL).
    pub ended: Vec<Entry>,
    /// Entries of dead owners whose child was already gone — or whose pid
    /// now belongs to another process: forgotten, nothing signalled.
    pub forgotten: Vec<Entry>,
    /// Entries whose owner still runs, left as they are.
    pub kept: usize,
}

/// End the children whose owner is gone and drop their entries. Entries
/// of a live owner are left alone. `grace` is the SIGTERM-to-SIGKILL bound
/// per child.
pub fn sweep(path: &Path, grace: Duration) -> std::io::Result<Sweep> {
    let mut sweep = Sweep::default();
    let mut orphans = Vec::new();
    locked(path, |entries| {
        let before = entries.len();
        entries.retain(|entry| {
            let owner_alive = entry.owner.is_alive();
            if !owner_alive {
                orphans.push(entry.clone());
            }
            owner_alive
        });
        sweep.kept = entries.len();
        entries.len() != before
    })?;
    // Outside the lock: a live Baaz recording a spawn never waits on a
    // sweep's grace period.
    for entry in orphans {
        if entry.child.is_alive() {
            end_group(entry.child.pid, grace);
            sweep.ended.push(entry);
        } else {
            sweep.forgotten.push(entry);
        }
    }
    Ok(sweep)
}

/// Every entry in the ledger at `path` (empty when it is missing or
/// unreadable).
pub fn read(path: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Vec::new();
    }
    let identity = |object: &Value, pid: &str, started: &str| -> Option<Identity> {
        Some(Identity {
            pid: i32::try_from(object.get(pid)?.as_i64()?).ok()?,
            started: object.get(started)?.as_u64()?,
        })
    };
    value
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|child| {
                    Some(Entry {
                        child: identity(child, "pid", "started")?,
                        program: child.get("program").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        owner: identity(child, "owner", "owner_started")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Write `entries` to `path`: a temp file renamed over it.
fn write(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    let children: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "pid": entry.child.pid,
                "started": entry.child.started,
                "program": entry.program,
                "owner": entry.owner.pid,
                "owner_started": entry.owner.started,
            })
        })
        .collect();
    let text = serde_json::to_string_pretty(&json!({"version": VERSION, "children": children}))
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let temp = path.with_extension(format!("json.tmp{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)
}

/// Read the entries at `path` under an exclusive lock, let `change` edit
/// them, and write them back when it says it changed something.
fn locked(path: &Path, change: impl FnOnce(&mut Vec<Entry>) -> bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))?;
    // SAFETY: `flock` on an fd we own; the lock ends when `lock` closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut entries = read(path);
    if change(&mut entries) {
        write(path, &entries)?;
    }
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("run").join(FILE_NAME)
    }

    #[test]
    fn records_round_trip_and_forget_removes_one() {
        let path = temp("round-trip");
        let owner = Identity { pid: 1, started: 7 };
        let ledger = Ledger::new(path.clone(), owner);
        let a = Identity { pid: 100, started: 1 };
        let b = Identity { pid: 101, started: 2 };
        ledger.record(a, "/bin/a").expect("record");
        ledger.record(b, "/bin/b").expect("record");
        ledger.record(a, "/bin/a").expect("a second record replaces the first");
        let entries = read(&path);
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries.contains(&Entry { child: a, program: "/bin/a".into(), owner }));
        ledger.forget(a).expect("forget");
        assert_eq!(read(&path).iter().map(|entry| entry.child).collect::<Vec<_>>(), [b]);
        let _ = std::fs::remove_dir_all(path.parent().and_then(Path::parent).expect("dir"));
    }

    #[test]
    fn a_garbled_or_foreign_file_reads_as_empty() {
        let path = temp("garbled");
        std::fs::create_dir_all(path.parent().expect("run")).expect("dir");
        std::fs::write(&path, "{not json").expect("write");
        assert!(read(&path).is_empty());
        std::fs::write(&path, r#"{"version": 99, "children": [{"pid": 1}]}"#).expect("write");
        assert!(read(&path).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().and_then(Path::parent).expect("dir"));
    }

    /// A live owner's entries survive a sweep untouched — a second Baaz on
    /// the same directory, or this one, keeps its children.
    #[test]
    fn a_live_owners_entries_are_kept() {
        let path = temp("live-owner");
        let me = Identity::current().expect("our identity");
        let ledger = Ledger::new(path.clone(), me);
        // Our own pid as the "child": alive, and must not be signalled.
        ledger.record(me, "self").expect("record");
        let swept = sweep(&path, Duration::from_millis(10)).expect("sweep");
        assert_eq!(swept, Sweep { kept: 1, ..Sweep::default() });
        assert_eq!(read(&path).len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().and_then(Path::parent).expect("dir"));
    }
}
