//! The one place that spells the `harness: …` stderr prefix.
//!
//! Every hand-rolled `eprintln!("harness: …")` site used to retype the
//! prefix itself (finding `app-core-16` / `A-MECH-12`); this macro is the
//! only place it is spelled now, so a future change to where these lines go
//! — or whether they carry a timestamp, a level, anything — is one edit.

/// Print one line to stderr, prefixed `harness: `.
#[macro_export]
macro_rules! harness_log {
    ($($arg:tt)*) => {
        eprintln!("harness: {}", format_args!($($arg)*))
    };
}

/// Whether `HARNESS_TRACE=1` switch tracing is on, read once.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("HARNESS_TRACE").is_ok_and(|v| v == "1"))
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
/// A no-op unless `HARNESS_TRACE=1`.
pub fn trace_reset() {
    if !trace_enabled() {
        return;
    }
    if let Ok(mut origin) = TRACE_ORIGIN.get_or_init(|| std::sync::Mutex::new(None)).lock() {
        *origin = Some(std::time::Instant::now());
    }
}

/// Print one switch-trace line to stderr: microseconds since [`trace_reset`]
/// and the label. A no-op unless `HARNESS_TRACE=1`, and silent until the
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
        eprintln!("harness-trace +{elapsed}us {label}");
    }
}

/// Arm the `first-content` mark for the next `render_transcript` (see
/// [`trace_first_frame`]). A no-op unless `HARNESS_TRACE=1`.
pub fn trace_arm_first_frame() {
    if trace_enabled() {
        TRACE_NEED_FIRST_FRAME.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Mark `first-content` once, on the first `render_transcript` after
/// [`trace_arm_first_frame`]. A no-op unless armed (and unless
/// `HARNESS_TRACE=1`).
pub fn trace_first_frame() {
    if TRACE_NEED_FIRST_FRAME.swap(false, std::sync::atomic::Ordering::Relaxed) {
        trace_mark("first-content");
    }
}
