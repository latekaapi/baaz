//! Fault injection on the `muse serve` transport (docs/82 5.1), against
//! stand-in children written here — never a real `muse`.
//!
//! - A child SIGKILLed under a request: the request fails at once (not at
//!   its 180 s deadline), the client says the child closed, and a request
//!   made after fails at once (docs/81 B PM-9).
//! - A child whose escaped grandchild keeps the pipes open: hanging up
//!   returns promptly — the reader and stderr threads are detached, never
//!   joined (docs/81 B PM-5).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use muse_client::{MuseClient, MuseConfig, MuseError, MuseEvent};

/// A stand-in `muse` in its own temp dir: `body` is the shell script after
/// the shebang. It ignores the `serve` arguments.
fn stand_in(name: &str, body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("muse-client-faults-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let program = dir.join("muse-stand-in");
    std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).expect("script");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, program)
}

fn config(program: &Path) -> MuseConfig {
    MuseConfig { program: program.to_owned(), trust_workspace: false, no_session_log: false, extra_args: Vec::new() }
}

/// SIGKILL `pid` (a stand-in of this test) if it still runs.
fn kill(pid: i32) {
    // SAFETY: a plain syscall on a stand-in this test started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

#[test]
fn a_killed_child_fails_its_request_at_once_and_every_later_one() {
    // Reads every request, answers none.
    let (dir, program) = stand_in("killed", "exec cat >/dev/null");
    let client = std::sync::Arc::new(MuseClient::spawn(&config(&program)).expect("spawns"));
    let events = client.events();
    let pid = client.pid() as i32;
    let asking = std::sync::Arc::clone(&client);
    let request = std::thread::spawn(move || asking.request("session/list", None));
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    kill(pid);
    let answer = request.join().expect("the request returns");
    assert!(matches!(answer, Err(MuseError::Closed)), "{answer:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "failed at the exit, not the deadline");
    let closed = events.recv_timeout(Duration::from_secs(5)).expect("the close is reported");
    assert!(matches!(closed, MuseEvent::Closed(None)), "killed by a signal, so no code: {closed:?}");
    let started = Instant::now();
    assert!(matches!(client.request("session/list", None), Err(MuseError::Closed)));
    assert!(started.elapsed() < Duration::from_millis(500), "a request after the close fails at once");
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_grandchild_holding_the_pipes_never_hangs_the_hang_up() {
    // The grandchild starts a session of its own (so the group kill misses
    // it, as a detached tool's would), keeps stdout and stderr open, and
    // says its pid. The child itself reads stdin and answers nothing.
    let (dir, program) = stand_in(
        "grandchild",
        "python3 -c 'import os, sys, time\nos.setsid()\nopen(sys.argv[1], \"w\").write(str(os.getpid()))\ntime.sleep(30)' \"$BAAZ_GRANDCHILD_PID\" &\nexec cat >/dev/null",
    );
    let pid_file = dir.join("grandchild.pid");
    let client = MuseClient::spawn_with_env(
        &config(&program),
        &[],
        &[("BAAZ_GRANDCHILD_PID".into(), pid_file.display().to_string())],
    )
    .expect("spawns");
    let deadline = Instant::now() + Duration::from_secs(10);
    let grandchild = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file).ok().and_then(|text| text.trim().parse::<i32>().ok()) {
            break pid;
        }
        assert!(Instant::now() < deadline, "the grandchild never started — is python3 on PATH?");
        std::thread::sleep(Duration::from_millis(20));
    };
    let started = Instant::now();
    drop(client);
    let took = started.elapsed();
    kill(grandchild);
    assert!(took < Duration::from_secs(3), "the hang-up joined a thread the grandchild keeps alive: {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
