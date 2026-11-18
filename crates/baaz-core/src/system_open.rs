//! `open(1)`, the one way Baaz hands a path or a URL to macOS (Finder,
//! the default browser, another app).
//!
//! The child is waited on a background thread, so a reveal never leaves a
//! zombie behind and never blocks the UI thread (docs/81 A F23). No
//! `pre_exec`: `Command::spawn` stays on `posix_spawn` (docs/82 rule 5).

use std::ffi::OsStr;
use std::process::{Command, Stdio};

/// Run `open <args>` detached and reap it in the background. `Err` only
/// when the process could not start.
pub fn open_detached<I, S>(args: I) -> std::io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new("/usr/bin/open")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::Builder::new()
        .name("baaz-open-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map(|_| ())
}
