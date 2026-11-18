//! The root render counter behind `BAAZ_TRACE=1` ([`note_root_render`]):
//! it reads the aui motion crate's frame requests, so it lives with the
//! window. Every other log, trace and breadcrumb is [`baaz_core::log`],
//! re-exported here so `crate::log::…` names both.

pub use baaz_core::log::*;

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

