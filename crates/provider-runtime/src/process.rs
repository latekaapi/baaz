//! A provider's child process: its own session, killed as a group.
//!
//! [`ChildProcess::spawn`] starts the program through
//! [`crate::session_spawn`] (`posix_spawn` with Apple's `SETSID`; never
//! `pre_exec`, which forks Baaz): the child leads a new session and process
//! group, so the tools and servers it starts (a Bash tool, an MCP server)
//! are in that group too. [`ChildProcess::terminate`] signals the whole
//! group — SIGTERM, then SIGKILL after a bound — and only then reaps the
//! leader (docs/81 B PM-2).
//!
//! **The pid is never reaped before the group is signalled.** Until the
//! leader is reaped its pid cannot be reused, and a group id is the leader's
//! pid, so every group signal reaches this child's group and nothing else.
//! Exit is observed without reaping (`waitid` with `WNOWAIT`), so asking
//! "has it exited?" never gives the pid away.
//!
//! Every child is recorded in the pid ledger ([`crate::ledger`]) when one is
//! installed, and forgotten once reaped: a Baaz that died without reaping
//! leaves its children there for the next launch to sweep.
//!
//! What escapes: a descendant that starts a session of its own (`setsid`,
//! Node's `detached: true`) is in no group of ours.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long [`ChildProcess::terminate`] gives the group to leave on
/// SIGTERM before SIGKILL: what every adapter's hang-up uses. Codex needs
/// the SIGTERM: a SIGKILLed app-server keeps the writer lease on the thread
/// it serves, and the next `thread/resume` of that thread waited ~5 s and
/// failed with "already has an active writer" (measured 2026-10-18,
/// codex-cli 0.153.2); SIGTERM released it in ~10 ms. The others get the
/// same courtesy.
pub const TERM_GRACE: Duration = Duration::from_millis(250);

/// How long a group gets to vanish after SIGKILL before a hang-up stops
/// waiting (a process in an uninterruptible wait cannot be hurried).
const KILL_WAIT: Duration = Duration::from_secs(2);

/// How a child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this code.
    Code(i32),
    /// A signal ended it.
    Signal(i32),
}

impl Exit {
    /// The exit code, when it exited rather than being signalled.
    pub fn code(self) -> Option<i32> {
        match self {
            Self::Code(code) => Some(code),
            Self::Signal(_) => None,
        }
    }
}

/// Who a process is: its pid and when it started. A pid alone is not an
/// identity — pids are reused — but a pid and its start time are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The pid.
    pub pid: i32,
    /// Its start time, microseconds since the Unix epoch.
    pub started: u64,
}

impl Identity {
    /// The identity of the live process `pid`, `None` when there is none
    /// (or its start time cannot be read).
    pub fn of(pid: i32) -> Option<Self> {
        start_time(pid).map(|started| Self { pid, started })
    }

    /// This process.
    pub fn current() -> Option<Self> {
        Self::of(std::process::id() as i32)
    }

    /// Whether the process `pid` is still the one this identity names: it
    /// exists and started at the same moment. A reused pid is a different
    /// process.
    pub fn is_alive(&self) -> bool {
        start_time(self.pid) == Some(self.started)
    }
}

/// When `pid` started, in microseconds since the Unix epoch.
#[cfg(target_os = "macos")]
pub fn start_time(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value of a plain C
    // struct; `proc_pidinfo` writes at most `size` bytes into it.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a valid, writable `proc_bsdinfo` of `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, std::ptr::addr_of_mut!(info).cast(), size)
    };
    (written == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

/// Elsewhere there is no start time to read, so no identity can be proven.
#[cfg(not(target_os = "macos"))]
pub fn start_time(_pid: i32) -> Option<u64> {
    None
}

/// Whether `pgid` may be signalled at all: never pid 1 or below (`kill(-1)`
/// is every process this user owns), never this process's own group.
fn signallable(pgid: i32) -> bool {
    // SAFETY: plain getters.
    let (own_pid, own_group) = unsafe { (libc::getpid(), libc::getpgrp()) };
    pgid > 1 && pgid != own_pid && pgid != own_group
}

/// Whether any live process is in group `pgid`. A zombie leader is not
/// live: macOS answers `EPERM` for a group of zombies, `ESRCH` for none.
pub(crate) fn group_alive(pgid: i32) -> bool {
    // SAFETY: signal 0 checks only; nothing is delivered.
    signallable(pgid) && unsafe { libc::kill(-pgid, 0) } == 0
}

/// Send `signal` to every process in group `pgid`.
pub(crate) fn signal_group(pgid: i32, signal: libc::c_int) {
    if signallable(pgid) {
        // SAFETY: a plain syscall; callers only name a group they proved
        // is theirs (see the module docs).
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
}

/// SIGTERM the group, wait up to `grace` for it to empty, then SIGKILL
/// whatever is left and wait (bounded) for that too. True when the group
/// is gone.
pub(crate) fn end_group(pgid: i32, grace: Duration) -> bool {
    if !group_alive(pgid) {
        return true;
    }
    signal_group(pgid, libc::SIGTERM);
    if wait_until(grace, || !group_alive(pgid)) {
        return true;
    }
    signal_group(pgid, libc::SIGKILL);
    wait_until(KILL_WAIT, || !group_alive(pgid))
}

/// End several groups at once, the way [`end_group`] ends one: SIGTERM to
/// every live group, one shared wait of up to `grace` for them all to empty,
/// then SIGKILL to whatever is left and a bounded wait for that too. True
/// when every group is gone. What a terminal task's Stop and the quit sweep
/// use: a group that ignores SIGTERM costs `grace` once, not once per group.
///
/// The groups are not this process's children: callers name a group they
/// read from a live terminal moments ago ([`foreground_group`]), whose
/// leader the shell has not reaped while the group still runs.
pub fn end_groups(pgids: &[i32], grace: Duration) -> bool {
    let live: Vec<i32> = pgids.iter().copied().filter(|pgid| group_alive(*pgid)).collect();
    if live.is_empty() {
        return true;
    }
    for pgid in &live {
        signal_group(*pgid, libc::SIGTERM);
    }
    if wait_until(grace, || live.iter().all(|pgid| !group_alive(*pgid))) {
        return true;
    }
    for pgid in &live {
        signal_group(*pgid, libc::SIGKILL);
    }
    wait_until(KILL_WAIT, || live.iter().all(|pgid| !group_alive(*pgid)))
}

/// The process group in the foreground of the terminal `session_leader`
/// controls — the command running at its prompt — when it is not the
/// leader's own group. `None` while the shell sits at its prompt (the shell
/// holds the foreground itself), when the leader is gone, or when it has no
/// terminal. Read from the kernel's process table (`proc_pidinfo`): no
/// process is started.
#[cfg(target_os = "macos")]
pub fn foreground_group(session_leader: i32) -> Option<i32> {
    if session_leader <= 1 {
        return None;
    }
    // SAFETY: an all-zero `proc_bsdinfo` is a valid value of a plain C
    // struct; `proc_pidinfo` writes at most `size` bytes into it.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a valid, writable `proc_bsdinfo` of `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(session_leader, libc::PROC_PIDTBSDINFO, 0, std::ptr::addr_of_mut!(info).cast(), size)
    };
    if written != size {
        return None;
    }
    let foreground = i32::try_from(info.e_tpgid).ok()?;
    let own = i32::try_from(info.pbi_pgid).ok()?;
    (foreground > 1 && foreground != own && signallable(foreground)).then_some(foreground)
}

/// Elsewhere there is no process table to read.
#[cfg(not(target_os = "macos"))]
pub fn foreground_group(_session_leader: i32) -> Option<i32> {
    None
}

/// Poll `done` every few milliseconds until it holds or `limit` passes.
fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What [`ChildProcess::spawn`] hands back: the process and the parent's
/// ends of its three pipes.
pub struct Spawned {
    /// The child.
    pub process: ChildProcess,
    /// Its stdin (write end).
    pub stdin: File,
    /// Its stdout (read end).
    pub stdout: File,
    /// Its stderr (read end).
    pub stderr: File,
}

#[derive(Default)]
struct State {
    /// The exit, once observed (without reaping) or reaped.
    exit: Option<Exit>,
    /// The leader has been reaped: its pid is no longer ours to signal.
    reaped: bool,
}

/// A running child that leads its own session and group. Shareable
/// (`&self` everywhere): a hang-up never needs a lock someone else holds
/// for long. Dropping it hangs up ([`Self::terminate`] with
/// [`TERM_GRACE`]).
pub struct ChildProcess {
    identity: Identity,
    program: String,
    state: Mutex<State>,
    /// Serialises hang-ups: one terminates, a second waits for it.
    hanging_up: Mutex<()>,
}

impl ChildProcess {
    /// Spawn `command` in a new session with all three stdio piped: its
    /// program, arguments, environment (this process's with the command's
    /// `env`/`env_remove` applied) and working directory. The command's own
    /// stdio settings are not used. A bare program name is looked up on the
    /// `PATH` the child will get, as `Command::spawn` does.
    pub fn spawn(command: &Command) -> std::io::Result<Spawned> {
        let env = command_env(command);
        let program = resolve(command.get_program(), &env);
        let args: Vec<OsString> = command.get_args().map(OsStr::to_owned).collect();
        let (pid, stdin, stdout, stderr) = spawn_piped(&program, &args, &env, command.get_current_dir())?;
        let identity = Identity { pid, started: start_time(pid).unwrap_or(0) };
        let program = program.to_string_lossy().into_owned();
        if let Some(ledger) = crate::ledger::installed() {
            if let Err(error) = ledger.record(identity, &program) {
                eprintln!("baaz: could not record child {pid} in the pid ledger: {error}");
            }
        }
        let process = ChildProcess { identity, program, state: Mutex::new(State::default()), hanging_up: Mutex::new(()) };
        Ok(Spawned { process, stdin, stdout, stderr })
    }

    /// The child's pid, which is also its session and group id.
    pub fn pid(&self) -> u32 {
        self.identity.pid as u32
    }

    /// The child's identity (pid and start time).
    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// The program that was started.
    pub fn program(&self) -> &str {
        &self.program
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// How the child ended, if it has — observed without reaping it, so its
    /// pid stays reserved for the group signal of a later hang-up.
    pub fn try_exit(&self) -> Option<Exit> {
        let mut state = self.state();
        if state.exit.is_none() && !state.reaped {
            state.exit = peek_exit(self.identity.pid);
        }
        state.exit
    }

    /// Whether the child has exited.
    pub fn has_exited(&self) -> bool {
        self.try_exit().is_some()
    }

    /// Wait up to `timeout` for the child to exit; `None` if it still runs.
    /// Never reaps and never blocks past `timeout`.
    pub fn wait_exit(&self, timeout: Duration) -> Option<Exit> {
        let mut exit = None;
        wait_until(timeout, || {
            exit = self.try_exit();
            exit.is_some()
        });
        exit
    }

    /// Hang up: SIGTERM the whole group, SIGKILL whatever is left after
    /// `grace`, then reap the leader and forget it in the ledger.
    /// Idempotent. Blocks for at most `grace` plus a bounded wait after the
    /// SIGKILL.
    pub fn terminate(&self, grace: Duration) {
        let _one = self.hanging_up.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.state().reaped {
            return;
        }
        let pid = self.identity.pid;
        // The leader is unreaped, so its pid — the group id — is still ours.
        end_group(pid, grace);
        // The group is empty (or past saving); the leader is dead or dying.
        // Reap it, bounded: an uninterruptible process cannot be hurried,
        // and a later hang-up (or the ledger sweep) tries again.
        let mut status = 0;
        let reaped = wait_until(KILL_WAIT, || {
            // SAFETY: waits on our own unreaped child, without blocking.
            let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            rc == pid || rc == -1
        });
        if !reaped {
            return;
        }
        let mut state = self.state();
        state.reaped = true;
        state.exit.get_or_insert(exit_of_status(status));
        drop(state);
        if let Some(ledger) = crate::ledger::installed() {
            let _ = ledger.forget(self.identity);
        }
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.terminate(TERM_GRACE);
    }
}

/// The exit of a child that has ended, without reaping it (`WNOWAIT`).
fn peek_exit(pid: i32) -> Option<Exit> {
    // SAFETY: an all-zero `siginfo_t` is a valid out-parameter.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: waits on our own child without reaping it or blocking.
    let rc = unsafe {
        libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
    };
    if rc != 0 || info.si_pid == 0 {
        return None;
    }
    Some(if info.si_code == libc::CLD_EXITED { Exit::Code(info.si_status) } else { Exit::Signal(info.si_status) })
}

/// A `waitpid` status as an [`Exit`].
fn exit_of_status(status: libc::c_int) -> Exit {
    if libc::WIFSIGNALED(status) {
        Exit::Signal(libc::WTERMSIG(status))
    } else {
        Exit::Code(libc::WEXITSTATUS(status))
    }
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

/// `program` as spawned: a bare name is looked up on the child's `PATH`
/// (what `Command::spawn` does when the command sets `PATH`), else handed
/// to `posix_spawnp` as it is.
fn resolve(program: &OsStr, env: &[(OsString, OsString)]) -> OsString {
    if program.as_encoded_bytes().contains(&b'/') {
        return program.to_owned();
    }
    let path = env.iter().find(|(key, _)| key == "PATH").map(|(_, value)| value.clone());
    path.and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join(program)).find(|candidate| is_executable(candidate))
    })
    .map(PathBuf::into_os_string)
    .unwrap_or_else(|| program.to_owned())
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// A pipe, both ends close-on-exec in this process.
#[cfg(target_os = "macos")]
fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` holds two ints for `pipe` to fill.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both fds were just returned by `pipe` and belong to nothing
    // else; each `OwnedFd` closes its own.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&read, &write] {
        // SAFETY: a valid fd we own.
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((read, write))
}

/// Spawn in a new session with stdin, stdout and stderr on fresh pipes.
/// Returns the pid and the parent's ends.
#[cfg(target_os = "macos")]
fn spawn_piped(
    program: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: Option<&Path>,
) -> std::io::Result<(i32, File, File, File)> {
    use crate::session_spawn::{spawn_in_new_session, FileAction};
    let (stdin_read, stdin_write) = pipe()?;
    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let mut files = vec![
        FileAction::Dup2 { from: stdin_read.as_raw_fd(), to: 0 },
        FileAction::Dup2 { from: stdout_write.as_raw_fd(), to: 1 },
        FileAction::Dup2 { from: stderr_write.as_raw_fd(), to: 2 },
    ];
    if let Some(cwd) = cwd {
        files.push(FileAction::Chdir(cwd.as_os_str().to_owned()));
    }
    let pid = spawn_in_new_session(program, args, env, &files)?;
    // The child's ends are the child's now: ours close here.
    drop((stdin_read, stdout_write, stderr_write));
    Ok((pid, File::from(stdin_write), File::from(stdout_read), File::from(stderr_read)))
}

/// Elsewhere (no `POSIX_SPAWN_SETSID`): its own process group through
/// `Command::process_group`, which needs no `pre_exec`.
#[cfg(not(target_os = "macos"))]
fn spawn_piped(
    program: &OsStr,
    args: &[OsString],
    env: &[(OsString, OsString)],
    cwd: Option<&Path>,
) -> std::io::Result<(i32, File, File, File)> {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    let mut command = Command::new(program);
    command.args(args).env_clear().envs(env.iter().map(|(k, v)| (k, v))).process_group(0);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn()?;
    let take = |fd: OwnedFd| File::from(fd);
    let stdin = take(OwnedFd::from(child.stdin.take().expect("piped")));
    let stdout = take(OwnedFd::from(child.stdout.take().expect("piped")));
    let stderr = take(OwnedFd::from(child.stderr.take().expect("piped")));
    let pid = child.id() as i32;
    // Reaped through `ChildProcess` from here on.
    std::mem::forget(child);
    Ok((pid, stdin, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    fn sh(script: &str) -> Spawned {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        ChildProcess::spawn(&command).expect("sh spawns")
    }

    #[test]
    fn the_child_leads_its_own_group_with_its_env_and_cwd() {
        let dir = std::env::temp_dir().join(format!("baaz-runtime-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let dir = dir.canonicalize().expect("canonical");
        let mut command = Command::new("sh");
        command
            .args(["-c", "printf '%s %s %s %s' \"$$\" \"$(ps -o pgid= -p $$ | tr -d ' ')\" \"$BAAZ_RT\" \"$(pwd -P)\""])
            .env("BAAZ_RT", "set")
            .current_dir(&dir);
        let mut spawned = ChildProcess::spawn(&command).expect("spawn");
        let mut out = String::new();
        spawned.stdout.read_to_string(&mut out).expect("stdout");
        let pid = spawned.process.pid().to_string();
        assert_eq!(out, format!("{pid} {pid} set {}", dir.display()));
        assert_eq!(spawned.process.wait_exit(Duration::from_secs(5)), Some(Exit::Code(0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stdin_reaches_the_child_and_exit_is_seen_without_reaping() {
        let mut spawned = sh("read line; echo \"got $line\"; exit 3");
        spawned.stdin.write_all(b"hello\n").expect("write");
        let mut out = String::new();
        spawned.stdout.read_to_string(&mut out).expect("stdout");
        assert_eq!(out, "got hello\n");
        assert_eq!(spawned.process.wait_exit(Duration::from_secs(5)), Some(Exit::Code(3)));
        // Peeked, not reaped: the pid is still held (a zombie), so it can
        // still be asked about.
        assert_eq!(spawned.process.try_exit(), Some(Exit::Code(3)));
        // SAFETY: signal 0 on our own unreaped child.
        assert_eq!(unsafe { libc::kill(spawned.process.identity().pid, 0) }, 0, "still ours until reaped");
        spawned.process.terminate(TERM_GRACE);
        assert_eq!(spawned.process.try_exit(), Some(Exit::Code(3)));
    }

    #[test]
    fn a_missing_program_fails_to_spawn() {
        let error = ChildProcess::spawn(&Command::new("/nonexistent/baaz-no-such-program")).err().expect("fails");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_polite_child_leaves_on_sigterm_without_waiting_out_the_grace() {
        let spawned = sh("exec sleep 30");
        let started = Instant::now();
        spawned.process.terminate(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(2), "SIGTERM ended it");
        assert_eq!(spawned.process.try_exit(), Some(Exit::Signal(libc::SIGTERM)));
    }

    #[test]
    fn end_groups_kills_a_group_that_ignores_sigterm_after_the_grace() {
        // The shell and both sleeps ignore SIGTERM (an ignored signal stays
        // ignored across exec), so only the SIGKILL after the grace ends them.
        let stubborn = sh("trap '' TERM; sleep 60 & sleep 60");
        let polite = sh("exec sleep 60");
        let (a, b) = (stubborn.process.pid() as i32, polite.process.pid() as i32);
        // Let the stubborn shell set its trap and start both sleeps.
        assert!(wait_until(Duration::from_secs(5), || {
            std::process::Command::new("pgrep").args(["-g", &a.to_string()]).output().is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout).lines().count() >= 3
            })
        }));
        let grace = Duration::from_millis(300);
        let started = Instant::now();
        assert!(end_groups(&[a, b], grace), "both groups end");
        assert!(started.elapsed() >= grace, "the stubborn group waited out the grace");
        assert!(!group_alive(a) && !group_alive(b), "no process of either group survives");
        assert_eq!(polite.process.wait_exit(Duration::from_secs(2)), Some(Exit::Signal(libc::SIGTERM)));
        assert_eq!(stubborn.process.wait_exit(Duration::from_secs(2)), Some(Exit::Signal(libc::SIGKILL)));
        // Gone groups need nothing.
        assert!(end_groups(&[a, b], grace));
    }

    #[test]
    fn a_shell_at_its_prompt_has_no_foreground_group_of_another() {
        // No terminal at all: nothing is in its foreground.
        let plain = sh("exec sleep 30");
        assert_eq!(foreground_group(plain.process.pid() as i32), None);
        assert_eq!(foreground_group(0), None);
        assert_eq!(foreground_group(1), None, "never pid 1");
    }

    #[test]
    fn identity_reads_this_process_and_rejects_a_wrong_start_time() {
        let me = Identity::current().expect("our own start time");
        assert!(me.is_alive());
        assert!(!Identity { started: me.started + 1, ..me }.is_alive(), "same pid, other start: not us");
    }
}
