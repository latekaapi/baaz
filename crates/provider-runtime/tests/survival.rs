//! "No child survives a killed Baaz" (docs/82 5.1), as a process test.
//!
//! A parent stand-in — this test binary re-run as [`parent_stand_in`] —
//! installs the pid ledger, spawns a stubborn stand-in child through
//! [`ChildProcess`] (it ignores SIGTERM and leaves a grandchild that does
//! too), and is then SIGKILLed: nothing hangs its child up. The launch
//! sweep against that ledger must end the child and the grandchild, and
//! must not touch a ledger entry whose pid now belongs to someone else (the
//! same pid with another start time).

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use provider_runtime::ledger::{self, Ledger, FILE_NAME};
use provider_runtime::{ChildProcess, Identity, Spawned};

mod support;
use support::{gone_within, stand_in, stand_in_command, Reap};

/// Set (to a directory) only for the re-run parent.
const PARENT_ENV: &str = "BAAZ_RUNTIME_PARENT_STAND_IN";

/// The parent stand-in: a no-op in a normal run. Re-run with
/// [`PARENT_ENV`] set it plays a Baaz: ledger installed, one child spawned
/// and never hung up, pids written to `<dir>/pids`, then it waits to be
/// killed.
#[test]
fn parent_stand_in() {
    let Some(dir) = std::env::var_os(PARENT_ENV).map(PathBuf::from) else { return };
    ledger::install(dir.join("run").join(FILE_NAME)).expect("the ledger installs");
    let Spawned { process, stdout, .. } = ChildProcess::spawn(&stand_in_command("stubborn")).expect("spawns");
    let mut grand = String::new();
    BufReader::new(stdout).read_line(&mut grand).expect("the grandchild's pid");
    let child = process.identity();
    let temp = dir.join("pids.tmp");
    std::fs::write(&temp, format!("{} {} {}", child.pid, child.started, grand.trim())).expect("pids");
    std::fs::rename(&temp, dir.join("pids")).expect("pids");
    // A Baaz that dies never hangs up: the child must outlive this process.
    std::mem::forget(process);
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn wait_for(path: &Path, limit: Duration) -> Option<String> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            return Some(text);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn no_child_survives_a_killed_parent_and_a_recycled_pid_is_left_alone() {
    let dir = std::env::temp_dir().join(format!("baaz-runtime-survival-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("run").join(FILE_NAME);

    let mut parent = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "parent_stand_in", "--test-threads", "1"])
        .env(PARENT_ENV, &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the parent stand-in starts");
    let parent_identity = Identity::of(parent.id() as i32).expect("the parent runs");
    let pids = match wait_for(&dir.join("pids"), Duration::from_secs(20)) {
        Some(pids) => pids,
        None => {
            let _ = parent.kill();
            let _ = parent.wait();
            panic!("the parent stand-in never reported its child");
        }
    };
    let fields: Vec<&str> = pids.split_whitespace().collect();
    let child = Identity { pid: fields[0].parse().expect("pid"), started: fields[1].parse().expect("start") };
    let grand = Identity::of(fields[2].parse().expect("pid")).expect("the grandchild runs");
    let _reap = Reap::of(child).and(grand);

    let recorded = ledger::read(&path);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].child, child);
    assert_eq!(recorded[0].owner, parent_identity);

    // The parent dies the way a crashed Baaz does: no hang-up runs.
    let _ = parent.kill();
    let _ = parent.wait();
    assert!(!parent_identity.is_alive());
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.is_alive() && grand.is_alive(), "orphaned and still running: what the sweep is for");

    // A pid the ledger names but that now belongs to another process: a
    // live group leader of ours, recorded under a start time it does not
    // have, by the same dead owner.
    let decoy = stand_in("deaf");
    let decoy_identity = decoy.process.identity();
    let _reap_decoy = Reap::process(&decoy.process);
    let recycled = Identity { pid: decoy_identity.pid, started: decoy_identity.started.saturating_sub(1_000_000) };
    Ledger::new(path.clone(), parent_identity).record(recycled, "recycled").expect("record");

    let swept = ledger::sweep(&path, Duration::from_millis(200)).expect("sweep");
    assert_eq!(swept.ended.iter().map(|entry| entry.child).collect::<Vec<_>>(), [child], "{swept:?}");
    assert_eq!(swept.forgotten.iter().map(|entry| entry.child).collect::<Vec<_>>(), [recycled], "{swept:?}");
    assert_eq!(swept.kept, 0);
    assert!(gone_within(child, Duration::from_secs(5)), "the orphaned child is gone");
    assert!(gone_within(grand, Duration::from_secs(5)), "and its grandchild");
    assert!(decoy_identity.is_alive(), "the recycled pid was never signalled");
    assert!(ledger::read(&path).is_empty(), "the swept entries are gone from the ledger");

    decoy.process.terminate(Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&dir);
}
