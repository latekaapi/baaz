//! Crash reports you can reach (docs/82 9.5). Nothing here ever leaves the
//! Mac: there is no network reporting of any kind.
//!
//! * A Rust panic writes `<state>/crashes/panic-<ms>.txt` from the panic
//!   hook ([`install_panic_hook`]): the message, where, the backtrace, the
//!   version and the OS.
//! * A native crash (a signal, an abort in the engine) leaves macOS's own
//!   report in `~/Library/Logs/DiagnosticReports/Baaz-*.ips`; the next launch
//!   copies the newest one that is newer than anything kept
//!   ([`collect_system_reports`]).
//! * The last [`KEEP`] reports are kept; older ones go.
//! * The next launch says once that Baaz quit unexpectedly ([`unseen`] /
//!   [`mark_seen`]); Settings › Data keeps the last report, with Reveal and
//!   Copy.

use std::path::{Path, PathBuf};

/// The folder under the state dir.
pub const DIR: &str = "crashes";
/// How many reports are kept.
pub const KEEP: usize = 5;
/// The file that holds the newest report's name the person has been told of.
const SEEN: &str = ".seen";

/// `<state>/crashes`.
pub fn dir(state: &Path) -> PathBuf {
    state.join(DIR)
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Write one panic report and return its path. Best-effort: a report that
/// cannot be written is lost, never a second panic.
pub fn write_panic_report(state: &Path, message: &str, location: &str, backtrace: &str) -> Option<PathBuf> {
    let folder = dir(state);
    std::fs::create_dir_all(&folder).ok()?;
    let path = folder.join(format!("panic-{}.txt", now_ms()));
    let text = format!(
        "Baaz {} quit unexpectedly (a panic).\n\nWhat: {message}\nWhere: {location}\nmacOS: {}\n\nBacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        os_version()
    );
    std::fs::write(&path, text).ok()?;
    prune(&folder);
    Some(path)
}

fn os_version() -> String {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Install the panic hook: the report is written, then the previous hook
/// (the default message on stderr) runs as before.
pub fn install_panic_hook(state: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_owned());
        let location = info.location().map(|at| format!("{}:{}", at.file(), at.line())).unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        if let Some(path) = write_panic_report(&state, &message, &location, &backtrace) {
            crate::baaz_log!("crash: report written to {}", path.display());
        }
        previous(info);
    }));
}

/// The running bundle's identifier, read from the `Info.plist` beside the
/// executable (`None` for an unbundled binary).
pub fn own_bundle_id() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let plist = std::fs::read_to_string(exe.parent()?.parent()?.join("Info.plist")).ok()?;
    let after = &plist[plist.find("<key>CFBundleIdentifier</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_owned())
}

/// The `bundleID` in a macOS report's one-line JSON header.
fn report_bundle_id(path: &Path) -> Option<String> {
    use std::io::BufRead as _;
    let mut header = String::new();
    std::io::BufReader::new(std::fs::File::open(path).ok()?).read_line(&mut header).ok()?;
    let value: serde_json::Value = serde_json::from_str(header.trim()).ok()?;
    value.get("bundleID")?.as_str().map(str::to_owned)
}

/// Copy macOS's newest report for this Baaz (`Baaz-*.ips` in `system_dir`
/// whose header names `bundle_id`) into the crashes folder when it is newer
/// than every report kept. Returns the copy's path. Another build's crash
/// (the check build's, `sh.baaz.check`) is never this one's to tell of.
pub fn collect_system_reports(state: &Path, system_dir: &Path, bundle_id: Option<&str>) -> Option<PathBuf> {
    let newest_kept = kept(state).first().and_then(|path| modified(path));
    let newest = std::fs::read_dir(system_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("Baaz") && name.ends_with(".ips"))
        })
        .filter(|path| report_bundle_id(path).as_deref() == bundle_id)
        .filter_map(|path| modified(&path).map(|at| (at, path)))
        .max_by_key(|(at, _)| *at)?;
    if newest_kept.is_some_and(|kept| kept >= newest.0) {
        return None;
    }
    let folder = dir(state);
    std::fs::create_dir_all(&folder).ok()?;
    let copy = folder.join(newest.1.file_name()?);
    if copy.exists() {
        return None;
    }
    std::fs::copy(&newest.1, &copy).ok()?;
    prune(&folder);
    Some(copy)
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The reports kept, newest first.
pub fn kept(state: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir(state)) else { return Vec::new() };
    let mut reports: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.')))
        .filter_map(|path| modified(&path).map(|at| (at, path)))
        .collect();
    reports.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    reports.into_iter().map(|(_, path)| path).collect()
}

/// The newest report, when there is one.
pub fn latest(state: &Path) -> Option<PathBuf> {
    kept(state).into_iter().next()
}

/// The newest report when the person has not been told of it yet.
pub fn unseen(state: &Path) -> Option<PathBuf> {
    let latest = latest(state)?;
    let seen = std::fs::read_to_string(dir(state).join(SEEN)).unwrap_or_default();
    let name = latest.file_name()?.to_string_lossy().into_owned();
    (seen.trim() != name).then_some(latest)
}

/// The person has been told of `report`.
pub fn mark_seen(state: &Path, report: &Path) {
    if let Some(name) = report.file_name() {
        let _ = std::fs::write(dir(state).join(SEEN), name.to_string_lossy().as_bytes());
    }
}

fn prune(folder: &Path) {
    let state = folder.parent().unwrap_or(folder);
    for old in kept(state).into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        dir
    }

    #[test]
    fn a_report_is_kept_told_once_and_pruned_to_five() {
        let state = temp("keep");
        let first = write_panic_report(&state, "boom", "src/x.rs:1", "frames").expect("written");
        let text = std::fs::read_to_string(&first).expect("reads");
        assert!(text.contains("boom") && text.contains("src/x.rs:1") && text.contains("frames"));
        assert_eq!(unseen(&state), Some(first.clone()), "the next launch tells");
        mark_seen(&state, &first);
        assert_eq!(unseen(&state), None, "once");
        for n in 0..7 {
            std::thread::sleep(std::time::Duration::from_millis(3));
            write_panic_report(&state, &format!("boom {n}"), "", "");
        }
        assert_eq!(kept(&state).len(), KEEP);
        assert!(unseen(&state).is_some(), "a newer report is told of again");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn the_newest_system_report_is_copied_once() {
        let state = temp("system-state");
        let system = temp("system-reports");
        let ours = "{\"app_name\":\"Baaz\",\"bundleID\":\"sh.baaz\"}\n{ native crash }";
        std::fs::write(system.join("Baaz-2026-11-25-010101.ips"), ours).expect("ips");
        std::fs::write(system.join("Other-2026-11-25-010101.ips"), "not ours").expect("other");
        let copy = collect_system_reports(&state, &system, Some("sh.baaz")).expect("copied");
        assert_eq!(std::fs::read_to_string(&copy).expect("reads"), ours);
        assert_eq!(collect_system_reports(&state, &system, Some("sh.baaz")), None, "not twice");
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&system);
    }

    /// Gate 11: the check build's forced crash (`sh.baaz.check`) is not the
    /// owner's Baaz's to tell of.
    #[test]
    fn another_builds_report_is_left_alone() {
        let state = temp("other-build-state");
        let system = temp("other-build-reports");
        std::fs::write(system.join("Baaz-2026-11-25-050502.ips"), "{\"app_name\":\"Baaz\",\"bundleID\":\"sh.baaz.check\"}\n{}").expect("ips");
        assert_eq!(collect_system_reports(&state, &system, Some("sh.baaz")), None);
        assert!(collect_system_reports(&state, &system, Some("sh.baaz.check")).is_some(), "the check build's own");
        let _ = std::fs::remove_dir_all(&state);
        let _ = std::fs::remove_dir_all(&system);
    }
}
