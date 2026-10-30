//! The one place that spells the `baaz: …` stderr prefix.
//!
//! Every hand-rolled `eprintln!("baaz: …")` site used to retype the
//! prefix itself (finding `app-core-16` / `A-MECH-12`); this macro is the
//! only place it is spelled now, so a future change to where these lines go
//! — or whether they carry a timestamp, a level, anything — is one edit.

/// Print one line to stderr, prefixed `baaz: `, and append it to the
/// state dir's `logs/baaz.log` once [`init_file_log`] has run.
///
/// A Finder-launched Baaz has no stderr anyone reads: a hang on
/// 2026-10-31 left no line behind. The file copy goes through a bounded
/// channel to a writer thread, so the call site pays one format and one
/// non-blocking send (a full channel drops the line, never waits).
#[macro_export]
macro_rules! baaz_log {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        eprintln!("baaz: {line}");
        $crate::log::file_line(&line);
    }};
}

/// The log file's name inside `<state dir>/logs/`.
pub const LOG_FILE: &str = "baaz.log";
/// Rotate past this many bytes: `baaz.log` becomes `baaz.log.1` (the old
/// `.1` is dropped), so the logs never hold more than ~2 × this.
pub const LOG_CAP_BYTES: u64 = 5 * 1024 * 1024;
/// Lines queued for the writer before new ones are dropped.
const LOG_QUEUE: usize = 1024;

/// The writer's inbox, set once by [`init_file_log`].
static LOG_TX: std::sync::OnceLock<std::sync::mpsc::SyncSender<String>> = std::sync::OnceLock::new();

/// Start appending every [`baaz_log!`] line (and the stall watchdog's) to
/// `<dir>/logs/baaz.log` on a writer thread. Idempotent: the first call
/// wins. Never called by tests (only `main` calls it), and a capture's
/// state dir is already a throwaway one (`store::support_dir`).
pub fn init_file_log(dir: &std::path::Path) {
    let logs = dir.join("logs");
    if std::fs::create_dir_all(&logs).is_err() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(LOG_QUEUE);
    if LOG_TX.set(tx).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("baaz-log".into()).spawn(move || {
        let mut file = RotatingLog::open(logs.join(LOG_FILE), LOG_CAP_BYTES);
        while let Ok(line) = rx.recv() {
            file.append(&line);
        }
    });
}

/// Queue one line for the log file, stamped with local time. A no-op
/// before [`init_file_log`] (tests, `--perf-*`), and when the queue is
/// full.
pub fn file_line(line: &str) {
    if let Some(tx) = LOG_TX.get() {
        let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
        let _ = tx.try_send(format!("{stamp} {line}"));
    }
}

/// An append-only text file that rotates to `<name>.1` past `cap` bytes.
pub struct RotatingLog {
    path: std::path::PathBuf,
    cap: u64,
    file: Option<std::fs::File>,
    size: u64,
}

impl RotatingLog {
    /// Open (or create) `path` for appending; an unopenable file makes
    /// every append a no-op rather than an error.
    pub fn open(path: std::path::PathBuf, cap: u64) -> Self {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        let size = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        Self { path, cap, file, size }
    }

    /// Append `line` and a newline, rotating first when the file is full.
    pub fn append(&mut self, line: &str) {
        use std::io::Write as _;
        if self.size >= self.cap {
            self.rotate();
        }
        if let Some(file) = self.file.as_mut() {
            if file.write_all(line.as_bytes()).and_then(|()| file.write_all(b"\n")).is_ok() {
                self.size += line.len() as u64 + 1;
            }
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        let _ = std::fs::rename(&self.path, &old);
        self.file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.size = 0;
    }
}

/// The last breadcrumb the UI thread left (see [`crumb`]): what it was
/// doing when the stall watchdog finds it silent.
static LAST_CRUMB: std::sync::Mutex<&'static str> = std::sync::Mutex::new("boot");

/// Leave a breadcrumb: a static label for what the UI thread is about to
/// do. Always on — one uncontended lock and a pointer store.
pub fn crumb(label: &'static str) {
    if let Ok(mut slot) = LAST_CRUMB.lock() {
        *slot = label;
    }
}

/// The last breadcrumb left by [`crumb`].
pub fn last_crumb() -> &'static str {
    LAST_CRUMB.lock().map(|slot| *slot).unwrap_or("?")
}

/// The last `BAAZ_TRACE` mark printed (empty when tracing is off).
static LAST_TRACE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// The last `BAAZ_TRACE` switch-trace mark, for the stall watchdog.
pub fn last_trace_mark() -> String {
    LAST_TRACE.lock().map(|mark| mark.clone()).unwrap_or_default()
}

/// Whether `BAAZ_TRACE=1` switch tracing is on, read once.
pub(crate) fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BAAZ_TRACE").is_ok_and(|v| v == "1"))
}

/// Process start for the cold-boot sidebar trace. Set once from `main`.
static BOOT_ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Record process start. Idempotent; the first call wins.
pub fn boot_init() {
    let _ = BOOT_ORIGIN.get_or_init(std::time::Instant::now);
}

/// Milliseconds since [`boot_init`]. `None` before init.
pub fn boot_ms() -> Option<u128> {
    BOOT_ORIGIN.get().map(|at| at.elapsed().as_millis())
}

/// One cold-boot timing line on stderr, behind `BAAZ_TRACE=1`.
pub fn boot_mark(label: &str) {
    if !trace_enabled() {
        return;
    }
    if let Some(ms) = boot_ms() {
        eprintln!("baaz-boot +{ms}ms {label}");
    }
}

/// Whether `BAAZ_HOVER_TRACE=1` hover-card tracing is on, read once.
pub(crate) fn hover_trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BAAZ_HOVER_TRACE").is_ok_and(|v| v == "1"))
}

/// Print one hover-trace line to stderr, prefixed `baaz-hover: `.
///
/// A no-op unless `BAAZ_HOVER_TRACE=1`; the format arguments are
/// evaluated only when enabled, so call sites stay cheap when off.
#[macro_export]
macro_rules! hover_trace {
    ($($arg:tt)*) => {
        if $crate::log::hover_trace_enabled() {
            eprintln!("baaz-hover: {}", format_args!($($arg)*))
        }
    };
}

/// The click (a sidebar row, or `--session` at boot) every trace mark is
/// measured from. Reset by [`trace_reset`]; read by [`trace_mark`].
static TRACE_ORIGIN: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
    std::sync::OnceLock::new();

/// Armed by the session swap; the next `render_transcript` consumes it and
/// marks `first-content`, so the mark means a frame really drew the new view.
static TRACE_NEED_FIRST_FRAME: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Start a switch trace: the origin every later [`trace_mark`] measures from.
///
/// A no-op unless `BAAZ_TRACE=1`.
pub fn trace_reset() {
    if !trace_enabled() {
        return;
    }
    if let Ok(mut origin) = TRACE_ORIGIN.get_or_init(|| std::sync::Mutex::new(None)).lock() {
        *origin = Some(std::time::Instant::now());
    }
}

/// Print one switch-trace line to stderr: microseconds since [`trace_reset`]
/// and the label. A no-op unless `BAAZ_TRACE=1`, and silent until the
/// first [`trace_reset`] (a backfill with no switch behind it traces nothing).
pub fn trace_mark(label: &str) {
    if !trace_enabled() {
        return;
    }
    let elapsed = TRACE_ORIGIN
        .get()
        .and_then(|origin| origin.lock().ok())
        .and_then(|origin| origin.as_ref().map(|at| at.elapsed().as_micros()));
    if let Some(elapsed) = elapsed {
        eprintln!("baaz-trace +{elapsed}us {label}");
        if let Ok(mut last) = LAST_TRACE.lock() {
            last.clear();
            last.push_str(label);
        }
    }
}

/// Arm the `first-content` mark for the next `render_transcript` (see
/// [`trace_first_frame`]). A no-op unless `BAAZ_TRACE=1`.
pub fn trace_arm_first_frame() {
    if trace_enabled() {
        TRACE_NEED_FIRST_FRAME.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Mark `first-content` once, on the first `render_transcript` after
/// [`trace_arm_first_frame`]. A no-op unless armed (and unless
/// `BAAZ_TRACE=1`).
pub fn trace_first_frame() {
    if TRACE_NEED_FIRST_FRAME.swap(false, std::sync::atomic::Ordering::Relaxed) {
        trace_mark("first-content");
    }
}

/// Root renders counted for [`note_root_render`]'s once-every-two-seconds
/// line, and when the window they count started.
static RENDER_WINDOW: std::sync::Mutex<Option<(std::time::Instant, u64)>> = std::sync::Mutex::new(None);

/// Count one root render; behind `BAAZ_TRACE=1`, print how many happened in
/// each two seconds that had any (`baaz-perf renders=N/2s`), then which call
/// sites asked for frames meanwhile (`baaz-perf frames=N by file:line=n …`,
/// from `aui_motion::frames`). No line means none: a window at rest should
/// print nothing at all.
pub fn note_root_render() {
    if !trace_enabled() {
        return;
    }
    let Ok(mut slot) = RENDER_WINDOW.lock() else { return };
    let now = std::time::Instant::now();
    match slot.as_mut() {
        Some((start, count)) if now.duration_since(*start) < std::time::Duration::from_secs(2) => *count += 1,
        Some((start, count)) => {
            eprintln!("baaz-perf renders={count}/2s");
            // Who asked for those frames, by call site (aui-motion's
            // counter): a window that should be at rest names its culprit.
            if let Some(by) = aui_motion::frames::summary(&aui_motion::frames::take(), 5) {
                eprintln!("baaz-perf frames={by}");
            }
            *start = now;
            *count = 1;
        }
        None => *slot = Some((now, 1)),
    }
}

/// The send being traced: when Enter landed, and whether its first reply
/// has been painted yet.
static SEND_ORIGIN: std::sync::Mutex<Option<(std::time::Instant, bool)>> = std::sync::Mutex::new(None);

/// Start tracing a send (behind `BAAZ_TRACE=1`): Enter has just landed.
pub fn send_reset() {
    if trace_enabled() {
        if let Ok(mut origin) = SEND_ORIGIN.lock() {
            *origin = Some((std::time::Instant::now(), false));
        }
        eprintln!("baaz-send +0us submit");
    }
}

/// One stage of the traced send: microseconds since [`send_reset`].
pub fn send_mark(stage: &str) {
    if !trace_enabled() {
        return;
    }
    if let Some((at, _)) = SEND_ORIGIN.lock().ok().and_then(|origin| *origin) {
        eprintln!("baaz-send +{}us {stage}", at.elapsed().as_micros());
    }
}

/// Mark `first-reply-painted` once per traced send, from the frame that
/// first drew assistant output after it.
pub fn send_reply_painted() {
    if !trace_enabled() {
        return;
    }
    let Ok(mut origin) = SEND_ORIGIN.lock() else { return };
    if let Some((at, painted)) = origin.as_mut() {
        if !*painted {
            *painted = true;
            eprintln!("baaz-send +{}us first-reply-painted", at.elapsed().as_micros());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_file_rotates_past_its_cap_keeping_one_old_file() {
        let dir = std::env::temp_dir().join(format!("baaz-log-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let mut log = RotatingLog::open(path.clone(), 64);
        for n in 0..10 {
            log.append(&format!("line {n} padded to twenty"));
        }
        let current = std::fs::read_to_string(&path).unwrap();
        let old = std::fs::read_to_string(dir.join(format!("{LOG_FILE}.1"))).unwrap();
        assert!(current.len() as u64 <= 64 + 32, "the live file stays near its cap: {}", current.len());
        assert!(current.ends_with("line 9 padded to twenty\n"), "the newest line is in the live file");
        assert!(!old.is_empty(), "the rotated file holds the earlier lines");
        assert!(!dir.join(format!("{LOG_FILE}.1.1")).exists(), "only one old file is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_before_init_goes_nowhere() {
        // Tests never call `init_file_log`: the file copy is a no-op and
        // never touches a store.
        file_line("dropped");
        assert!(LOG_TX.get().is_none());
    }
}
