//! A login-shell `PATH` for every provider child.
//!
//! A GUI launch (notably from the Dock) gets launchd's minimal `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin`), while the provider CLIs live in home
//! installs (`~/.local/bin`, nvm node dirs, Homebrew prefixes). Spawning a
//! resolved absolute path still fails then: a `#!/usr/bin/env node` shebang
//! re-searches the child's `PATH` for `node`. So every provider spawn sets
//! its `PATH` to [`child_path_for`] of the resolved program, built on the
//! [`login_path`] below.
//!
//! [`login_path`] is computed once: the user's `$SHELL` (fallback
//! `/bin/zsh`) runs as a login interactive shell printing `$PATH` between
//! unique markers (rc files may print noise around it), with stdin null and
//! a hard timeout. On failure or timeout it falls back to
//! `/usr/libexec/path_helper -s`, then the current `PATH`. The known
//! fallback install dirs are always appended, de-duplicated, order
//! preserved.
//!
//! Tests bypass the shell with `BAAZ_LOGIN_PATH`: when set, its value is
//! used verbatim as the shell-derived portion instead of running a shell.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The env var that bypasses the shell: when set, its value is used
/// verbatim as the shell-derived portion of [`login_path`] (fallbacks are
/// still appended). Tests set this instead of running a login shell.
pub const LOGIN_PATH_ENV: &str = "BAAZ_LOGIN_PATH";

/// How long the login-shell probe may take before it is killed and the
/// fallbacks are used instead.
const SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// Markers around the `$PATH` print: shell rc files may print noise before
/// or after, so only what is between the markers counts.
const MARKER: &str = "__BAAZ_PATH__";

/// The shell script printing `$PATH` between [`MARKER`]s.
const SHELL_PROBE: &str = "printf '\n__BAAZ_PATH__%s__BAAZ_PATH__\n' \"$PATH\"";

/// The merged login-shell `PATH` value: shell (or its fallbacks) plus the
/// known install dirs, de-duplicated, order preserved. Computed once per
/// process.
pub fn login_path() -> &'static OsStr {
    static CACHED: OnceLock<OsString> = OnceLock::new();
    CACHED.get_or_init(login_path_uncached)
}

/// The same merge as [`login_path`], recomputed on every call so tests can
/// drive it with [`LOGIN_PATH_ENV`] without poisoning the cached value.
pub fn login_path_uncached() -> OsString {
    let shell = std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
    compute_login_path_with(&shell, SHELL_TIMEOUT, Path::new("/usr/libexec/path_helper"))
}

/// The `PATH` value for a provider child spawning `program`: the program's
/// own directory first (so a node script finds the `node` next to it),
/// then [`login_path`], de-duplicated, order preserved.
///
/// Nothing else about the child's environment is changed.
pub fn child_path_for(program: &Path) -> OsString {
    child_path_for_with(program, &split_login_path())
}

/// The merge behind [`child_path_for`], with the login dirs injected so
/// tests can drive it without touching the cached [`login_path`].
fn child_path_for_with(program: &Path, login_dirs: &[PathBuf]) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(parent) = program.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        push(parent.to_path_buf());
    }
    for dir in login_dirs {
        push(dir.clone());
    }
    join_dirs(&dirs)
}

/// Find `name` on [`login_path`]: the first entry whose `<dir>/<name>` is
/// an existing file. The executable bit is deliberately not checked, matching
/// the resolvers this replaces.
pub fn find_program(name: &str) -> Option<PathBuf> {
    find_program_in(name, &split_login_path())
}

/// The search behind [`find_program`], with the dirs injected so tests can
/// drive it without touching the cached [`login_path`].
fn find_program_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

/// The merge behind [`login_path_uncached`], with the shell, timeout and
/// `path_helper` injected so tests can drive the timeout path fast.
fn compute_login_path_with(shell: &Path, timeout: Duration, path_helper: &Path) -> OsString {
    let base = match std::env::var_os(LOGIN_PATH_ENV) {
        Some(overridden) => split_os_paths(&overridden),
        None => shell_path_dirs(shell, timeout)
            .or_else(|| path_helper_dirs(path_helper))
            .unwrap_or_else(current_path_dirs),
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut merged: Vec<PathBuf> = Vec::new();
    for dir in base.iter().chain(fallback_dirs(&home).iter()) {
        if !merged.contains(dir) {
            merged.push(dir.clone());
        }
    }
    join_dirs(&merged)
}

/// The fixed fallback install dirs plus every `~/.nvm/versions/node/*/bin`,
/// newest version first. Never empty-filtered: a dir that does not exist on
/// this machine simply never matches a lookup.
fn fallback_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".claude/local"),
    ];
    dirs.extend(nvm_bin_dirs(home));
    dirs
}

/// Every `$HOME/.nvm/versions/node/<version>/bin` that exists, newest
/// version first (numeric `major.minor.patch` descending; unparseable names
/// sort last, by name).
fn nvm_bin_dirs(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".nvm/versions/node");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut versions: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let bin = entry.path().join("bin");
        if bin.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            versions.push((name, bin));
        }
    }
    versions.sort_by(|a, b| {
        match (version_key(&a.0), version_key(&b.0)) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        }
    });
    versions.into_iter().map(|(_, bin)| bin).collect()
}

/// The numeric sort key for an nvm version directory name (`v22.22.2`).
/// Returns `None` when no leading numeric component parses.
fn version_key(name: &str) -> Option<(u64, u64, u64)> {
    let name = name.strip_prefix('v').unwrap_or(name);
    let mut parts = name.split('.');
    let mut next = || match parts.next() {
        None | Some("") => Some(0),
        Some(part) => part.parse::<u64>().ok(),
    };
    Some((next()?, next()?, next()?))
}

/// Run `shell -ilc <probe>` with stdin null, waiting at most `timeout`
/// (the child is killed on expiry). Returns the `$PATH` dirs printed
/// between the markers, or `None` on any failure, timeout, or unparseable
/// output — rc noise outside the markers is ignored.
fn shell_path_dirs(shell: &Path, timeout: Duration) -> Option<Vec<PathBuf>> {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new(shell);
    // A new SESSION (not just a process group): the probe shell leads its
    // own group, so a timeout kills whatever the rc files started along
    // with the shell (killing only the leader left a `sleep` orphaned); and
    // it has no controlling terminal, so an interactive shell run from a
    // terminal launch cannot be stopped by SIGTTOU/SIGTTIN for touching a
    // tty whose foreground group it is not in (a bare `process_group(0)`
    // stopped it and every terminal launch hit the timeout).
    // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .arg("-ilc")
        .arg(SHELL_PROBE)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut output = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_end(&mut output);
                }
                let _ = child.wait();
                return parse_shell_output(&String::from_utf8_lossy(&output))
                    .map(|value| split_os_paths(OsStr::new(value)));
            }
            Ok(Some(_)) => {
                let _ = child.wait();
                return None;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// SIGKILL every process in the group `pgid` (the probe shell leads its own
/// session and group, see [`shell_path_dirs`]).
fn kill_group(pgid: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        // SAFETY: a plain syscall on a group this module created.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// The `$PATH` printed between the [`MARKER`]s, ignoring any rc noise
/// before the first marker or after the second. `None` when the markers
/// are missing or the value between them is empty.
fn parse_shell_output(output: &str) -> Option<&str> {
    let (_, rest) = output.split_once(MARKER)?;
    let (value, _) = rest.split_once(MARKER)?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Parse `/usr/libexec/path_helper -s` output (`PATH="…"; export PATH; …`),
/// returning the quoted `PATH` value. `None` when the helper cannot run or
/// its output does not parse.
fn path_helper_dirs(path_helper: &Path) -> Option<Vec<PathBuf>> {
    let output = std::process::Command::new(path_helper).arg("-s").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_path_helper_output(&String::from_utf8_lossy(&output.stdout))
        .map(|value| split_os_paths(OsStr::new(value)))
}

/// The `PATH="…"` value in `path_helper -s` output. `None` when absent.
fn parse_path_helper_output(output: &str) -> Option<&str> {
    for chunk in output.split(';') {
        let chunk = chunk.trim();
        if let Some(value) = chunk.strip_prefix("PATH=") {
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// The process's current `PATH`, split. Empty when unset.
fn current_path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|paths| split_os_paths(&paths)).unwrap_or_default()
}

/// [`login_path`] split into directories.
fn split_login_path() -> Vec<PathBuf> {
    split_os_paths(login_path())
}

/// Split one `PATH`-shaped value. Never fails: entries that do not parse
/// are dropped.
fn split_os_paths(value: &OsStr) -> Vec<PathBuf> {
    std::env::split_paths(value).collect()
}

/// Join dirs into a `PATH`-shaped value. Never fails.
fn join_dirs(dirs: &[PathBuf]) -> OsString {
    std::env::join_paths(dirs).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review finding: a timeout killed only the shell and left what its rc
    /// started running. The fake shell backgrounds a long `sleep`, records
    /// its pid and never prints the markers; after the timeout the sleep
    /// must be gone too.
    #[test]
    fn a_timeout_kills_what_the_rc_started() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("baaz-envpath-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("sleep.pid");
        let shell = dir.join("fake-shell");
        std::fs::write(
            &shell,
            format!("#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n", pidfile.display()),
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        // 2.5 s, not less: macOS takes several hundred ms to first-exec a
        // freshly written script, and the rc must have run before the
        // timeout for there to be anything to orphan.
        assert_eq!(shell_path_dirs(&shell, Duration::from_millis(2500)), None);
        let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
        // `kill -0` succeeds only while the process exists.
        let mut alive = true;
        for _ in 0..50 {
            alive = std::process::Command::new("/bin/kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!alive, "the rc's background sleep {pid} outlived the timeout");
    }

    /// Serialize the tests that mutate the process environment: the
    /// runner executes tests on threads sharing one environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "provider-env-path-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");
        root
    }

    #[test]
    fn marker_parsing_ignores_rc_noise_before_and_after() {
        let output = "echo hello from .zshrc\nsome motd line\n__BAAZ_PATH__/a/bin:/b/bin__BAAZ_PATH__\nmesg: ttyname failed\n";
        assert_eq!(parse_shell_output(output), Some("/a/bin:/b/bin"));
    }

    #[test]
    fn marker_parsing_rejects_missing_or_empty_markers() {
        assert_eq!(parse_shell_output("no markers here"), None);
        assert_eq!(parse_shell_output("__BAAZ_PATH____BAAZ_PATH__"), None);
        assert_eq!(parse_shell_output("__BAAZ_PATH__   __BAAZ_PATH__"), None);
        assert_eq!(parse_shell_output("__BAAZ_PATH__/only-one"), None);
    }

    #[test]
    fn path_helper_output_parses_the_quoted_path() {
        let output = "PATH=\"/usr/bin:/bin:/usr/sbin:/sbin\"; export PATH;";
        assert_eq!(parse_path_helper_output(output), Some("/usr/bin:/bin:/usr/sbin:/sbin"));
        assert_eq!(parse_path_helper_output("nothing here"), None);
        assert_eq!(parse_path_helper_output("PATH=\"\"; export PATH;"), None);
    }

    #[test]
    fn a_hung_shell_falls_back_instead_of_hanging() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = temp_root("slow-shell");
        let shell = dir.join("slow-sh");
        std::fs::write(&shell, "#!/bin/sh\nsleep 30\n").expect("fake shell");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perms = std::fs::metadata(&shell).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shell, perms).expect("executable");
        }
        std::env::remove_var(LOGIN_PATH_ENV);
        let before = Instant::now();
        let value = compute_login_path_with(&shell, Duration::from_millis(200), &dir.join("no-helper"));
        // The fallback chain still answers: the current PATH plus the
        // install dirs, joined — never empty shell output, never a hang.
        assert!(before.elapsed() < Duration::from_secs(20), "the shell was killed on timeout");
        let text = value.to_string_lossy().into_owned();
        assert!(text.contains("/opt/homebrew/bin"), "fallbacks are appended: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_dedupes_keeping_first_order() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // A shell that can never run: the override below must win without
        // spawning anything.
        let no_shell = Path::new("/no/such/shell");
        std::env::set_var(LOGIN_PATH_ENV, "/a/bin:/b/bin:/a/bin");
        let value = compute_login_path_with(no_shell, Duration::from_millis(100), Path::new("/no/such/helper"));
        std::env::remove_var(LOGIN_PATH_ENV);
        let merged: Vec<PathBuf> = std::env::split_paths(&value).collect();
        let first_a = merged.iter().position(|dir| dir == &PathBuf::from("/a/bin")).expect("has /a/bin");
        assert_eq!(merged.iter().filter(|dir| *dir == &PathBuf::from("/a/bin")).count(), 1);
        assert!(merged[first_a + 1..].contains(&PathBuf::from("/b/bin")), "order kept: {merged:?}");
        // The fixed fallbacks land after the override's own entries.
        let homebrew = merged.iter().position(|dir| dir == &PathBuf::from("/opt/homebrew/bin"));
        assert!(homebrew.is_some_and(|index| index > first_a), "fallbacks appended: {merged:?}");
    }

    #[test]
    fn child_path_puts_the_programs_dir_first() {
        let login = vec![PathBuf::from("/a/bin"), PathBuf::from("/b/bin")];
        let program = PathBuf::from("/nvm/versions/node/v22.22.2/bin/codex");
        let value = child_path_for_with(&program, &login);
        let dirs: Vec<PathBuf> = std::env::split_paths(&value).collect();
        assert_eq!(dirs.first(), Some(&PathBuf::from("/nvm/versions/node/v22.22.2/bin")));
        assert!(dirs.contains(&PathBuf::from("/a/bin")));
        // The program dir appears exactly once even when the login path
        // already contains it.
        let login = vec![
            PathBuf::from("/nvm/versions/node/v22.22.2/bin"),
            PathBuf::from("/a/bin"),
        ];
        let value = child_path_for_with(&program, &login);
        let dirs: Vec<PathBuf> = std::env::split_paths(&value).collect();
        assert_eq!(
            dirs.iter().filter(|dir| *dir == &PathBuf::from("/nvm/versions/node/v22.22.2/bin")).count(),
            1
        );
        // A bare file name contributes no dir of its own.
        let value = child_path_for_with(Path::new("codex"), &login);
        let dirs: Vec<PathBuf> = std::env::split_paths(&value).collect();
        assert_eq!(dirs, login);
    }

    #[test]
    fn the_env_override_replaces_the_shell() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // A shell that would hang forever must never run while the
        // override is set.
        let dir = temp_root("override-shell");
        let shell = dir.join("slow-sh");
        std::fs::write(&shell, "#!/bin/sh\nsleep 30\n").expect("fake shell");
        std::env::set_var(LOGIN_PATH_ENV, "/override/bin");
        let value = compute_login_path_with(&shell, Duration::from_millis(100), &dir.join("no-helper"));
        let text = value.to_string_lossy().into_owned();
        assert!(text.starts_with("/override/bin"), "verbatim first: {text}");
        assert!(text.contains("/opt/homebrew/bin"), "fallbacks still appended: {text}");
        std::env::remove_var(LOGIN_PATH_ENV);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nvm_dirs_come_newest_first() {
        let home = temp_root("nvm-home");
        for version in ["v20.11.0", "v22.22.2", "v18.0.0"] {
            std::fs::create_dir_all(home.join(".nvm/versions/node").join(version).join("bin"))
                .expect("nvm dir");
        }
        let dirs = nvm_bin_dirs(&home);
        let names: Vec<String> = dirs
            .iter()
            .filter_map(|dir| dir.parent().and_then(|parent| parent.file_name()))
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["v22.22.2", "v20.11.0", "v18.0.0"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn find_program_searches_the_login_path() {
        let dir = temp_root("find-bin");
        std::fs::write(dir.join("faketool"), b"fake").expect("seed binary");
        let dirs = vec![PathBuf::from("/nothing/here"), dir.clone()];
        assert_eq!(find_program_in("faketool", &dirs), Some(dir.join("faketool")));
        assert_eq!(find_program_in("no-such-tool", &dirs), None);
        assert_eq!(find_program_in("faketool", &[]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
