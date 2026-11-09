//! Start a child in a NEW SESSION without forking Baaz.
//!
//! `Command::pre_exec(setsid)` — the obvious way to give a child its own
//! session — makes std `fork()` instead of `posix_spawn`. Forking Baaz, which
//! runs dozens of threads (Chromium's among them), runs libSystem's at-fork
//! handlers in the child while another thread may hold one of their locks:
//! the login-`PATH` probe's child aborted in `_os_once_gate_corruption_abort`
//! (libnotify's one-time initialiser) before it could exec, and macOS put up
//! a crash report each time (4 on 9 Nov 2026). Here the kernel makes the
//! session (`POSIX_SPAWN_SETSID`, an Apple extension) and nothing runs
//! between fork and exec. `POSIX_SPAWN_CLOEXEC_DEFAULT` keeps every Baaz fd
//! out of the child except the ones [`FileAction`]s place.
//!
//! A session leader that opens a terminal device acquires it as its
//! controlling terminal on macOS, so a TUI child gets its pty that way —
//! no `TIOCSCTTY` (checked: `ps -o tty=` shows the pty).

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;

/// One step the child's fd table takes before exec, in order.
#[derive(Debug, Clone)]
pub enum FileAction {
    /// Open `path` with `flags` as `fd`.
    Open {
        /// The descriptor the opened file lands on.
        fd: libc::c_int,
        /// What to open.
        path: OsString,
        /// `open(2)` flags.
        flags: libc::c_int,
    },
    /// Duplicate `from` onto `to`.
    Dup2 {
        /// An fd already in the child (placed by an earlier action, or a
        /// parent fd — which survives only through this dup).
        from: libc::c_int,
        /// Where it lands.
        to: libc::c_int,
    },
}

// <spawn.h>, Apple extensions (macOS 10.15+); `libc` does not export them.
const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;
const POSIX_SPAWN_CLOEXEC_DEFAULT: libc::c_short = 0x4000;

/// `posix_spawnp` `program` with `args` (not including argv[0]) and exactly
/// `env`, in a new session, its fds set up by `files`. Returns the pid, which
/// is also the session and process-group id. The caller reaps it.
pub fn spawn_in_new_session(
    program: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    files: &[FileAction],
) -> std::io::Result<libc::pid_t> {
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "a NUL byte in a spawn argument");
    let program = CString::new(program.as_bytes()).map_err(|_| invalid())?;
    let mut owned_args = vec![program.clone()];
    for arg in args {
        owned_args.push(CString::new(arg.as_bytes()).map_err(|_| invalid())?);
    }
    let mut argv: Vec<*mut libc::c_char> = owned_args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
    argv.push(std::ptr::null_mut());
    let mut owned_env = Vec::with_capacity(env.len());
    for (key, value) in env {
        let mut entry = key.as_bytes().to_vec();
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        owned_env.push(CString::new(entry).map_err(|_| invalid())?);
    }
    let mut envp: Vec<*mut libc::c_char> = owned_env.iter().map(|entry| entry.as_ptr().cast_mut()).collect();
    envp.push(std::ptr::null_mut());
    let mut paths = Vec::new();
    for action in files {
        if let FileAction::Open { path, .. } = action {
            paths.push(CString::new(path.as_bytes()).map_err(|_| invalid())?);
        }
    }

    let mut pid: libc::pid_t = 0;
    // SAFETY: the attr and file-action objects are initialised before use and
    // destroyed after; every pointer handed over (program, argv, envp, open
    // paths) outlives the call; argv and envp are NULL-terminated.
    let rc = unsafe {
        let mut actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut actions);
        libc::posix_spawnattr_init(&mut attr);
        let mut next_path = paths.iter();
        for action in files {
            match action {
                FileAction::Open { fd, flags, .. } => {
                    let path = next_path.next().expect("one CString per Open");
                    libc::posix_spawn_file_actions_addopen(&mut actions, *fd, path.as_ptr(), *flags, 0o644);
                }
                FileAction::Dup2 { from, to } => {
                    libc::posix_spawn_file_actions_adddup2(&mut actions, *from, *to);
                }
            }
        }
        libc::posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETSID | POSIX_SPAWN_CLOEXEC_DEFAULT);
        let rc = libc::posix_spawnp(&mut pid, program.as_ptr(), &actions, &attr, argv.as_ptr(), envp.as_ptr());
        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::posix_spawnattr_destroy(&mut attr);
        rc
    };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc));
    }
    Ok(pid)
}

/// The environment a `Command` would give its child: this process's, with
/// the command's own `env`/`env_remove` applied (`env_clear` is not used by
/// any caller and is not honoured).
pub fn command_env(command: &std::process::Command) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        env.retain(|(existing, _)| existing.as_os_str() != key);
        if let Some(value) = value {
            env.push((key.to_owned(), value.to_owned()));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(script: &str, files: &[FileAction]) -> libc::pid_t {
        spawn_in_new_session(
            OsStr::new("/bin/sh"),
            &[OsString::from("-c"), OsString::from(script)],
            &std::env::vars_os().collect::<Vec<_>>(),
            files,
        )
        .expect("spawn /bin/sh")
    }

    fn reap(pid: libc::pid_t) -> i32 {
        let mut status = 0;
        // SAFETY: waits on our own child.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        libc::WEXITSTATUS(status)
    }

    /// The child leads its own session and group, gets the env it was given,
    /// and its stdout lands where the file actions put it.
    #[test]
    fn the_child_leads_a_new_session_with_its_own_fds() {
        let dir = std::env::temp_dir().join(format!("baaz-session-spawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("out");
        let pid = run(
            "printf '%s %s %s' \"$$\" \"$(ps -o pgid= -p $$ | tr -d ' ')\" \"$(ps -o tty= -p $$ | tr -d ' ')\"",
            &[
                FileAction::Open { fd: 0, path: "/dev/null".into(), flags: libc::O_RDONLY },
                FileAction::Open { fd: 1, path: out.clone().into(), flags: libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC },
                FileAction::Open { fd: 2, path: "/dev/null".into(), flags: libc::O_WRONLY },
            ],
        );
        assert_eq!(reap(pid), 0);
        let text = std::fs::read_to_string(&out).expect("output");
        let parts: Vec<&str> = text.split(' ').collect();
        assert_eq!(parts[0], pid.to_string(), "{text}");
        assert_eq!(parts[1], pid.to_string(), "the child leads its own group: {text}");
        assert_eq!(parts[2], "??", "no controlling terminal: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opening a pty slave makes it the session leader's controlling terminal.
    #[test]
    fn a_pty_opened_by_the_leader_becomes_its_terminal() {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: out-parameters only.
        let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(rc, 0);
        // SAFETY: `slave` is open; `ttyname` returns a static C string.
        let name = unsafe { std::ffi::CStr::from_ptr(libc::ttyname(slave)) }.to_owned();
        let tty = OsStr::from_bytes(name.to_bytes()).to_owned();
        let pid = run(
            "[ \"$(ps -o tty= -p $$ | tr -d ' ')\" = \"$(basename \"$(tty)\")\" ] && [ \"$(ps -o tty= -p $$ | tr -d ' ')\" != '??' ]",
            &[
                FileAction::Open { fd: 0, path: tty, flags: libc::O_RDWR },
                FileAction::Dup2 { from: 0, to: 1 },
                FileAction::Dup2 { from: 0, to: 2 },
            ],
        );
        // SAFETY: both fds are ours.
        unsafe { libc::close(slave) };
        assert_eq!(reap(pid), 0, "the pty is the child's controlling terminal");
        // SAFETY: ours.
        unsafe { libc::close(master) };
    }

    /// `env` and `env_remove` on the command shape the child's environment.
    #[test]
    fn command_env_applies_overrides_and_removals() {
        let mut command = std::process::Command::new("/bin/true");
        command.env("BAAZ_SPAWN_TEST", "1").env_remove("HOME");
        let env = command_env(&command);
        assert!(env.iter().any(|(k, v)| k == "BAAZ_SPAWN_TEST" && v == "1"));
        assert!(!env.iter().any(|(k, _)| k == "HOME"));
    }
}
