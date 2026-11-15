//! One Baaz per state directory (docs/81 B D-4).
//!
//! Every store is a whole-file JSON map written by whichever process saved
//! last, so two windows on one state directory overwrite each other's names,
//! pins and session rows. The first launch takes an exclusive `flock` on
//! `<state>/lock` and writes its pid there; a second launch on the same
//! directory finds the lock held, brings the first forward and exits.
//!
//! The lock is the open file, held for the process's life and released by
//! the kernel when it dies, so a crash never leaves a stale lock. Rust opens
//! files close-on-exec, so no provider child inherits it.

use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::path::Path;

/// The held lock: keep it alive for the life of the process.
pub struct InstanceLock {
    _file: std::fs::File,
}

/// What [`acquire`] found.
pub enum Acquire {
    /// This process now owns the state directory.
    Owned(InstanceLock),
    /// Another process does; its pid when the lock file names one.
    HeldBy(Option<i32>),
}

/// Take the state directory's lock, or say who holds it.
pub fn acquire(dir: &Path) -> std::io::Result<Acquire> {
    std::fs::create_dir_all(dir)?;
    let mut file =
        std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("lock"))?;
    // SAFETY: a plain syscall on a descriptor this function owns.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            return Ok(Acquire::HeldBy(text.trim().parse().ok()));
        }
        return Err(error);
    }
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "{}", std::process::id())?;
    let _ = file.sync_all();
    Ok(Acquire::Owned(InstanceLock { _file: file }))
}

/// Bring the process `pid` to the front (its windows, ignoring other apps).
/// `false` when no running application has that pid.
pub fn bring_forward(pid: i32) -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2::{class, msg_send, runtime::AnyObject};
        // NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps
        const OPTIONS: usize = (1 << 0) | (1 << 1);
        // SAFETY: class methods on AppKit's NSRunningApplication; a nil
        // result (no such pid) is checked before the second send.
        unsafe {
            let app: *mut AnyObject =
                msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                return false;
            }
            let activated: bool = msg_send![app, activateWithOptions: OPTIONS];
            activated
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_ENV: &str = "BAAZ_TEST_LOCK_CHILD_DIR";

    /// Not a test on its own: the second process [`a_second_process_finds_the_lock_held`]
    /// spawns. Without the env var it does nothing.
    #[test]
    fn lock_child_helper() {
        let Some(dir) = std::env::var_os(CHILD_ENV) else { return };
        match acquire(Path::new(&dir)).expect("acquire") {
            Acquire::Owned(_) => println!("LOCK=owned"),
            Acquire::HeldBy(pid) => println!("LOCK=held:{}", pid.unwrap_or(-1)),
        }
    }

    fn child(dir: &Path) -> String {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "instance::tests::lock_child_helper", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, dir)
            .output()
            .expect("spawn the second process");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.split_whitespace().find_map(|word| word.strip_prefix("LOCK=").map(str::to_owned)).unwrap_or(text)
    }

    /// Two processes on one state directory: the second finds the first's
    /// pid; once the first lets go, a later process owns it.
    #[test]
    fn a_second_process_finds_the_lock_held() {
        let dir = std::env::temp_dir().join(format!("baaz-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let held = match acquire(&dir).expect("acquire") {
            Acquire::Owned(lock) => lock,
            Acquire::HeldBy(_) => panic!("a fresh directory is free"),
        };
        assert_eq!(child(&dir), format!("held:{}", std::process::id()));
        drop(held);
        assert_eq!(child(&dir), "owned", "the lock goes with its holder");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another directory is another instance.
    #[test]
    fn another_state_directory_is_free() {
        let a = std::env::temp_dir().join(format!("baaz-instance-a-{}", std::process::id()));
        let b = std::env::temp_dir().join(format!("baaz-instance-b-{}", std::process::id()));
        let _held = acquire(&a).expect("a");
        assert_eq!(child(&b), "owned");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
