//! One clock for everything a capture can see.
//!
//! Relative labels ("now", "2m") and elapsed rows read a clock, and two runs
//! taken seconds apart quantise differently — that is most of the
//! run-to-run capture drift. Every `Local::now()` / `Instant::now()` that
//! feeds a label or elapsed text goes through here; with
//! `HARNESS_DETERMINISTIC=1` the clocks freeze so a capture is byte-identical
//! run to run. Deadline and scheduling clocks (`shot.rs`, `tier.rs`, the
//! resize double-click) are untouched: they decide timing, never pixels.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

/// Whether `HARNESS_DETERMINISTIC=1` was in the environment, read once.
fn enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("HARNESS_DETERMINISTIC").as_deref() == Ok("1"))
}

/// Whether captures must be byte-identical run to run.
pub fn deterministic() -> bool {
    enabled()
}

/// The frozen monotonic instant, captured on first use under the flag.
fn frozen_instant() -> Instant {
    static FROZEN: OnceLock<Instant> = OnceLock::new();
    *FROZEN.get_or_init(Instant::now)
}

/// `Instant::now()`, frozen under the flag.
///
/// The same value comes back every call, so an elapsed duration derived
/// through [`elapsed_since`] is exactly zero: a "Working…" row shows no
/// elapsed cell and a countdown shows its full duration, on every run.
pub fn now_instant() -> Instant {
    if deterministic() {
        frozen_instant()
    } else {
        Instant::now()
    }
}

/// How long since `earlier`, where `earlier` came from [`now_instant`].
///
/// Zero under the flag, wall time otherwise.
pub fn elapsed_since(earlier: Instant) -> Duration {
    now_instant().duration_since(earlier)
}

/// The frozen wall clock, captured on first use under the flag.
fn frozen_local() -> DateTime<Local> {
    static FROZEN: OnceLock<DateTime<Local>> = OnceLock::new();
    *FROZEN.get_or_init(Local::now)
}

/// `Local::now()`, frozen under the flag.
///
/// Callers that label data (the sidebar) do not use this directly: they take
/// the newest timestamp in the data as "now" (see
/// [`crate::sidebar::grouping_now`]), so the newest row reads "now" however old
/// the fixture is. This is the fallback for rows with no data behind them.
pub fn now_local() -> DateTime<Local> {
    if deterministic() {
        frozen_local()
    } else {
        Local::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frozen_instant_never_advances_within_a_process() {
        // Without the flag this is wall time and could flake; only assert
        // the helper's own contract: `elapsed_since(now)` is zero when the
        // two reads agree, which they always do under the flag.
        if deterministic() {
            assert_eq!(elapsed_since(now_instant()), Duration::ZERO);
        }
    }
}
