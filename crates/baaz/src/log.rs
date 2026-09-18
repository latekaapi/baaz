//! The one place that spells the `baaz: …` stderr prefix.
//!
//! Every hand-rolled `eprintln!("baaz: …")` site used to retype the
//! prefix itself (finding `app-core-16` / `A-MECH-12`); this macro is the
//! only place it is spelled now, so a future change to where these lines go
//! — or whether they carry a timestamp, a level, anything — is one edit.

/// Print one line to stderr, prefixed `baaz: `.
#[macro_export]
macro_rules! baaz_log {
    ($($arg:tt)*) => {
        eprintln!("baaz: {}", format_args!($($arg)*))
    };
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
