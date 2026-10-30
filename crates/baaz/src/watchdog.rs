//! The main-thread stall watchdog.
//!
//! On 2026-10-31 the owner's Baaz (launched from Finder) stopped answering
//! for a while and recovered, leaving nothing behind: `baaz_log!` went to a
//! stderr nobody read and macOS wrote no hang report. Now every line also
//! lands in `<state dir>/logs/baaz.log` (see [`crate::log::init_file_log`]),
//! and this watchdog writes there when the UI thread goes quiet.
//!
//! How: the UI thread [`beat`]s from a foreground timer every
//! [`BEAT_EVERY`]; a background thread polls the last beat every
//! [`POLL_EVERY`]. A beat older than [`STALL_AFTER`] is a stall: one
//! `stall:` line with how long, whether the app was frontmost, how much CPU
//! the main thread burned meanwhile (busy vs blocked), the UI thread's last
//! breadcrumb ([`crate::log::crumb`]) and the last `BAAZ_TRACE` mark. A
//! stall that reaches [`SAMPLE_AFTER`] while the app is frontmost (or, in
//! the background, while the main thread is busy on CPU) also runs
//! `/usr/bin/sample` on the process once (at most every
//! [`SAMPLE_COOLDOWN`]), writing every thread's stack to
//! `logs/stall-<time>.txt`. The end of a stall writes `stall ended:`.
//!
//! Overhead: one timer wakeup per [`BEAT_EVERY`] on the UI thread (an
//! atomic store) and a sleeping thread. Only `main` starts it, and not for
//! deterministic captures; tests drive [`Detector`] and [`run_loop`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How often the UI thread beats.
pub const BEAT_EVERY: Duration = Duration::from_millis(100);
/// How often the watchdog looks.
pub const POLL_EVERY: Duration = Duration::from_millis(100);
/// A beat older than this is a stall.
pub const STALL_AFTER: Duration = Duration::from_millis(500);
/// A stall this long gets a `sample` of the whole process.
pub const SAMPLE_AFTER: Duration = Duration::from_millis(1_000);
/// At most one `sample` per this long.
pub const SAMPLE_COOLDOWN: Duration = Duration::from_secs(300);
/// Stall stack files kept in `logs/` (oldest removed first).
const KEEP_SAMPLES: usize = 5;

/// When the watchdog started: the clock beats are measured on.
static ORIGIN: OnceLock<Instant> = OnceLock::new();
/// Milliseconds since [`ORIGIN`] at the last beat.
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);
/// Whether a window of the app was active at the last beat.
static APP_ACTIVE: AtomicBool = AtomicBool::new(true);

fn now_ms() -> u64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The UI thread is alive. `active`: a window of the app is frontmost.
pub fn beat(active: bool) {
    LAST_BEAT_MS.store(now_ms(), Ordering::Relaxed);
    APP_ACTIVE.store(active, Ordering::Relaxed);
}

/// What one poll found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The UI thread has not beaten for `silent_ms`.
    Began { silent_ms: u64 },
    /// The stall has lasted long enough to sample the process.
    Sample { silent_ms: u64 },
    /// The UI thread beat again after `silent_ms` of silence.
    Ended { silent_ms: u64 },
}

/// The pure stall state machine: feed it the time and the last beat.
#[derive(Debug, Default)]
pub struct Detector {
    /// The beat the current stall started after, while one is on.
    stalled_after: Option<u64>,
    /// The longest silence seen in the current stall.
    longest: u64,
    /// Whether the current stall has been sampled.
    sampled: bool,
}

impl Detector {
    /// One poll at `now_ms` with the last beat at `beat_ms`.
    pub fn observe(&mut self, now_ms: u64, beat_ms: u64) -> Vec<Event> {
        let silent = now_ms.saturating_sub(beat_ms);
        let mut out = Vec::new();
        match self.stalled_after {
            Some(stalled) if beat_ms != stalled => {
                // A beat landed: the stall is over.
                out.push(Event::Ended { silent_ms: self.longest });
                self.stalled_after = None;
                self.sampled = false;
                self.longest = 0;
            }
            Some(_) => {
                self.longest = self.longest.max(silent);
                if !self.sampled && silent >= SAMPLE_AFTER.as_millis() as u64 {
                    self.sampled = true;
                    out.push(Event::Sample { silent_ms: silent });
                }
            }
            None if silent > STALL_AFTER.as_millis() as u64 => {
                self.stalled_after = Some(beat_ms);
                self.longest = silent;
                out.push(Event::Began { silent_ms: silent });
            }
            None => {}
        }
        out
    }
}

/// Poll `beat_ms` with `clock` every `poll` until `stop`, handing each
/// [`Event`] to `sink`. The watchdog thread's whole body; tests run it with
/// their own beat and sink.
pub fn run_loop(
    beat_ms: &AtomicU64,
    clock: impl Fn() -> u64,
    poll: Duration,
    stop: &AtomicBool,
    mut sink: impl FnMut(Event),
) {
    let mut detector = Detector::default();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(poll);
        for event in detector.observe(clock(), beat_ms.load(Ordering::Relaxed)) {
            sink(event);
        }
    }
}

/// The main thread's CPU time so far, in milliseconds (`None` off macOS or
/// when the kernel will not say).
#[cfg(target_os = "macos")]
fn main_cpu_ms(port: libc::mach_port_t) -> Option<u64> {
    #[allow(deprecated)]
    let mut info: libc::thread_basic_info = unsafe { std::mem::zeroed() };
    let mut count = libc::THREAD_BASIC_INFO_COUNT;
    let status = unsafe {
        libc::thread_info(
            port,
            libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
            (&mut info as *mut libc::thread_basic_info).cast(),
            &mut count,
        )
    };
    if status != 0 {
        return None;
    }
    let ms = |t: libc::time_value_t| t.seconds as u64 * 1000 + t.microseconds as u64 / 1000;
    Some(ms(info.user_time) + ms(info.system_time))
}

/// One `stall:` line: how long, frontmost or not, the main thread's CPU
/// meanwhile, and where the UI thread last said it was.
pub fn stall_line(silent_ms: u64, active: bool, cpu_ms: Option<u64>, crumb: &str, trace: &str) -> String {
    let cpu = cpu_ms.map(|ms| format!("; main thread CPU {ms} ms of it")).unwrap_or_default();
    let fore = if active { "app frontmost" } else { "app in background" };
    let trace = if trace.is_empty() { String::new() } else { format!("; last trace mark: {trace}") };
    format!("stall: UI thread silent {silent_ms} ms ({fore}{cpu}); last crumb: {crumb}{trace}")
}

/// Start the watchdog: the UI-thread beat on a foreground timer and the
/// polling thread. Call once from `main`, on the UI thread, after
/// [`crate::log::init_file_log`]. Never in tests or deterministic captures.
pub fn start(logs: std::path::PathBuf, cx: &mut gpui::App) {
    #[cfg(target_os = "macos")]
    #[allow(deprecated)]
    let main_port = unsafe { libc::mach_thread_self() };
    beat(true);
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(BEAT_EVERY).await;
        let active = cx.update(|cx| cx.active_window().is_some());
        beat(active);
    })
    .detach();
    let _ = std::thread::Builder::new().name("baaz-watchdog".into()).spawn(move || {
        static STOP: AtomicBool = AtomicBool::new(false);
        let mut cpu_at_begin: Option<u64> = None;
        let mut began_silent: u64 = 0;
        let mut last_sample: Option<Instant> = None;
        let mut child: Option<std::process::Child> = None;
        run_loop(&LAST_BEAT_MS, now_ms, POLL_EVERY, &STOP, |event| {
            #[cfg(target_os = "macos")]
            let cpu_now = main_cpu_ms(main_port);
            #[cfg(not(target_os = "macos"))]
            let cpu_now: Option<u64> = None;
            let active = APP_ACTIVE.load(Ordering::Relaxed);
            if let Some(running) = child.as_mut() {
                if !matches!(running.try_wait(), Ok(None)) {
                    child = None;
                }
            }
            match event {
                Event::Began { silent_ms } => {
                    cpu_at_begin = cpu_now;
                    began_silent = silent_ms;
                    crate::log::file_line(&stall_line(
                        silent_ms,
                        active,
                        None,
                        crate::log::last_crumb(),
                        &crate::log::last_trace_mark(),
                    ));
                }
                Event::Sample { silent_ms } => {
                    let due = last_sample.is_none_or(|at| at.elapsed() >= SAMPLE_COOLDOWN);
                    // In the background a late beat may only be App Nap
                    // coalescing the timer; sample there only when the main
                    // thread is visibly busy (at least half the time since
                    // the stall was noticed on CPU).
                    let busy = match (cpu_at_begin, cpu_now) {
                        (Some(a), Some(b)) => b.saturating_sub(a) * 2 >= silent_ms.saturating_sub(began_silent),
                        _ => false,
                    };
                    if (active || busy) && due && child.is_none() {
                        last_sample = Some(Instant::now());
                        let name = format!("stall-{}.txt", chrono::Local::now().format("%Y%m%d-%H%M%S"));
                        let path = logs.join(&name);
                        prune_samples(&logs);
                        child = std::process::Command::new("/usr/bin/sample")
                            .arg(std::process::id().to_string())
                            .args(["3", "-mayDie", "-file"])
                            .arg(&path)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .ok();
                        crate::log::file_line(&format!(
                            "stall: still silent at {silent_ms} ms; sampling every thread to logs/{name}"
                        ));
                    }
                }
                Event::Ended { silent_ms } => {
                    let cpu = match (cpu_at_begin.take(), cpu_now) {
                        (Some(a), Some(b)) => format!("; main thread CPU {} ms since it was noticed", b.saturating_sub(a)),
                        _ => String::new(),
                    };
                    crate::log::file_line(&format!(
                        "stall ended: UI thread silent ~{silent_ms} ms{cpu}; last crumb: {}",
                        crate::log::last_crumb()
                    ));
                }
            }
        });
    });
}

/// Keep at most [`KEEP_SAMPLES`] - 1 stall files before a new one lands.
fn prune_samples(logs: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(logs) else { return };
    let mut stalls: Vec<std::path::PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("stall-")))
        .collect();
    stalls.sort();
    while stalls.len() >= KEEP_SAMPLES {
        let _ = std::fs::remove_file(stalls.remove(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_quiet_ui_thread_begins_a_stall_samples_once_and_ends_on_the_next_beat() {
        let mut d = Detector::default();
        assert!(d.observe(100, 50).is_empty(), "a fresh beat is no stall");
        assert!(d.observe(500, 50).is_empty(), "450 ms is under the threshold");
        assert_eq!(d.observe(600, 50), vec![Event::Began { silent_ms: 550 }]);
        assert!(d.observe(1_000, 50).is_empty(), "a stall is reported once");
        assert_eq!(d.observe(2_100, 50), vec![Event::Sample { silent_ms: 2_050 }]);
        assert!(d.observe(3_000, 50).is_empty(), "sampled once per stall");
        assert_eq!(d.observe(3_100, 3_050), vec![Event::Ended { silent_ms: 2_950 }]);
        assert!(d.observe(3_200, 3_150).is_empty(), "beating again is quiet");
    }

    /// The thread body on a real clock: beat, go quiet for 700 ms, beat
    /// again — the sink sees the stall begin and end, and the line it
    /// would write names the crumb.
    #[test]
    fn the_watchdog_thread_writes_on_a_simulated_stall() {
        let beat = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let origin = Instant::now();
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let handle = {
            let (beat, stop, events) = (beat.clone(), stop.clone(), events.clone());
            std::thread::spawn(move || {
                run_loop(&beat, move || origin.elapsed().as_millis() as u64, Duration::from_millis(20), &stop, |e| {
                    events.lock().unwrap().push(e)
                })
            })
        };
        // Alive for a while.
        for _ in 0..5 {
            beat.store(origin.elapsed().as_millis() as u64, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(30));
        }
        // The simulated stall: no beat for 700 ms.
        std::thread::sleep(Duration::from_millis(700));
        beat.store(origin.elapsed().as_millis() as u64, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(80));
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let seen = events.lock().unwrap().clone();
        assert!(matches!(seen.first(), Some(Event::Began { silent_ms }) if *silent_ms > 500), "{seen:?}");
        assert!(matches!(seen.last(), Some(Event::Ended { silent_ms }) if *silent_ms >= 500), "{seen:?}");
        let line = stall_line(612, true, Some(598), "settings:render", "first-content");
        assert_eq!(
            line,
            "stall: UI thread silent 612 ms (app frontmost; main thread CPU 598 ms of it); last crumb: settings:render; last trace mark: first-content"
        );
        assert_eq!(
            stall_line(700, false, None, "render", ""),
            "stall: UI thread silent 700 ms (app in background); last crumb: render"
        );
    }
}
