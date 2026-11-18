//! The hub's respawn path (docs/82 5.1): paced, capped, never hung by an
//! old child's leftovers.
//!
//! The stand-in `muse` is the puppet (`tests/fixtures/model_list_puppet.py`)
//! behind a wrapper that first starts a grandchild in a session of its own
//! — out of reach of the group kill, the way a detached tool would be —
//! holding the child's stdout and stderr open. Every respawn replaces a
//! child whose pipes therefore never close: the replacement must not wait
//! on them (docs/81 B PM-5). Then the backoff: a few respawns, then the hub
//! gives up and says so (PM-7), and a person's Reconnect starts afresh. No
//! model is reached; nothing here is a real `muse`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use provider::runtime::RespawnBackoff;
use provider::{ConnectInfo, ProviderError};
use provider_muse::MuseHub;

/// The wrapper script in a fresh temp dir; grandchildren write their pids
/// into `dir` as `gc.<pid>`.
fn wrapper(dir: &Path) -> PathBuf {
    let puppet = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py");
    let program = dir.join("muse-wrapper");
    let grandchild = format!(
        "import os, time\nos.setsid()\nopen(os.path.join('{}', 'gc.%d' % os.getpid()), 'w').close()\ntime.sleep(20)",
        dir.display()
    );
    std::fs::write(
        &program,
        format!("#!/bin/sh\npython3 -c \"{grandchild}\" &\nexec python3 '{}' \"$@\"\n", puppet.display()),
    )
    .expect("wrapper");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    program
}

/// SIGKILL every grandchild the wrappers started.
fn kill_grandchildren(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(pid) = name.strip_prefix("gc.").and_then(|pid| pid.parse::<i32>().ok()) {
            // SAFETY: a stand-in grandchild this test's wrapper started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

#[test]
fn respawns_back_off_then_give_up_and_never_hang_on_an_old_child() {
    let dir = std::env::temp_dir().join(format!("provider-muse-respawn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let program = wrapper(&dir);
    let hub = MuseHub::connect(program.to_str().expect("utf-8"), &ConnectInfo::new("baaz", "0.1.0"))
        .expect("the stand-in shakes hands — is python3 on PATH?");
    hub.set_respawn_backoff(RespawnBackoff::new(
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_secs(60),
        3,
    ));

    // Two respawns go through, each replacing a child whose grandchild
    // holds its pipes: neither waits on those pipes.
    for round in 0..2 {
        let started = Instant::now();
        hub.reconnect().unwrap_or_else(|error| panic!("respawn {round}: {error}"));
        assert!(started.elapsed() < Duration::from_secs(5), "respawn {round} took {:?}", started.elapsed());
        assert!(hub.handshake().is_some(), "the link lock is free and the new child answered");
    }
    // The third crash inside the window: the wire is down, with the count.
    let refused = hub.reconnect().expect_err("gave up");
    assert!(
        matches!(&refused, ProviderError::Unavailable { reason } if reason.contains("3 times")),
        "{refused:?}"
    );
    // A person's Reconnect starts the count afresh.
    hub.forgive_crashes();
    hub.reconnect().expect("a forgiven respawn goes through");

    // Quit: the hang-up returns promptly and the hub never respawns again.
    let started = Instant::now();
    hub.shutdown();
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert!(hub.reconnect().is_err(), "a hung-up hub stays down");

    kill_grandchildren(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}
