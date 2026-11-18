//! The stand-in child and the clean-up every fault test shares.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use provider_runtime::{ChildProcess, Identity, Spawned};

/// The stand-in script. Every stand-in runs as `/bin/sh <this path> <mode>`,
/// so `pgrep -f provider-runtime/tests/support/stand-in.sh` finds any left
/// behind.
pub fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/stand-in.sh")
}

/// The stand-in running `mode`, through [`ChildProcess`].
pub fn stand_in(mode: &str) -> Spawned {
    ChildProcess::spawn(&stand_in_command(mode)).expect("the stand-in spawns")
}

/// The command that runs the stand-in in `mode`.
pub fn stand_in_command(mode: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg(script()).arg(mode);
    command
}

/// Whether `who` is gone (or no longer the process it named) by `limit`.
pub fn gone_within(who: Identity, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if !who.is_alive() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    !who.is_alive()
}

/// Kills what a test started if the test fails before it does: only
/// processes whose pid AND start time still match.
pub struct Reap(Vec<Identity>);

impl Reap {
    pub fn process(process: &ChildProcess) -> Self {
        Self(vec![process.identity()])
    }

    pub fn of(who: Identity) -> Self {
        Self(vec![who])
    }

    pub fn and(mut self, who: Identity) -> Self {
        self.0.push(who);
        self
    }
}

impl Drop for Reap {
    fn drop(&mut self) {
        for who in &self.0 {
            if who.is_alive() {
                // SAFETY: SIGKILL to a stand-in this test started, its
                // identity (pid + start time) just checked.
                unsafe { libc::kill(who.pid, libc::SIGKILL) };
            }
        }
    }
}
