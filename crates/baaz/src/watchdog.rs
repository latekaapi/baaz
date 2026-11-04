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
//! the main thread burned since its last beat (busy vs blocked), the UI
//! thread's last breadcrumb ([`crate::log::crumb`]) and the last `BAAZ_TRACE`
//! mark. A stall that reaches [`SAMPLE_AFTER`] also runs `/usr/bin/sample` on
//! the process once (at most every [`SAMPLE_COOLDOWN`]), writing every
//! thread's stack to `logs/stall-<time>.txt`. The end of a stall writes
//! `stall ended:`.
//!
//! **A silence in the background is usually not a stall.** macOS coalesces
//! and naps a background app's timers: over ~4 days the owner's log gathered
//! 434 `stall:` lines, every one "app in background", main-thread CPU 0–3 ms,
//! silent 1.6–2.1 s, and not one in the foreground. So a background silence
//! counts only when [`stall_is_real`] says so: the main thread was on CPU for
//! most of it, or it reached [`BACKGROUND_LONG`].
//!
//! The same beat notices a machine waking ([`woke`]): the wall clock runs
//! through a sleep and the monotonic clock does not. [`wake_channel`] hands
//! that on, for work that wants another go after one.
//!
//! Overhead: one timer wakeup per [`BEAT_EVERY`] on the UI thread (two clock
//! reads, one `thread_info`, atomic stores) and a sleeping thread. Only `main`
//! starts it, and not for deterministic captures; tests drive [`Detector`]
//! and [`run_loop`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

/// How often the UI thread beats. Once a second: a 100 ms beat cost a
/// visible idle Baaz ~0.3 % CPU (`idle.cpu_pct` 0.7–0.8 against its 0.5
/// budget, 31 Oct), and a freeze a person notices lasts seconds anyway.
pub const BEAT_EVERY: Duration = Duration::from_millis(1_000);
/// How often the watchdog looks (its own thread; costs the UI nothing).
pub const POLL_EVERY: Duration = Duration::from_millis(250);
/// A beat older than this is a stall: one and a half beats.
pub const STALL_AFTER: Duration = Duration::from_millis(1_500);
/// A stall this long gets a `sample` of the whole process.
pub const SAMPLE_AFTER: Duration = Duration::from_millis(2_000);
/// A silence in the background this long is reported whatever the CPU says:
/// App Nap does not delay a timer this far.
pub const BACKGROUND_LONG: Duration = Duration::from_secs(30);
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
/// The main thread's CPU time at the last beat, in milliseconds;
/// [`NO_CPU_READING`] when the kernel would not say.
static CPU_AT_BEAT_MS: AtomicU64 = AtomicU64::new(NO_CPU_READING);
const NO_CPU_READING: u64 = u64::MAX;
/// Who wants to hear about a wake ([`wake_channel`]).
static WAKE_LISTENERS: std::sync::Mutex<Vec<futures::channel::mpsc::UnboundedSender<()>>> =
    std::sync::Mutex::new(Vec::new());

fn now_ms() -> u64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The UI thread is alive. `active`: a window of the app is frontmost.
pub fn beat(active: bool) {
    LAST_BEAT_MS.store(now_ms(), Ordering::Relaxed);
    APP_ACTIVE.store(active, Ordering::Relaxed);
}

/// Whether a silence of `silent_ms` is worth a report. Frontmost it always
/// is. In the background the timers themselves get coalesced, so it is only
/// when the main thread was actually working through it (`cpu_ms`, its CPU
/// since the last beat, over half of the silence) or the silence is
/// [`BACKGROUND_LONG`]. No CPU reading is no evidence of work.
pub fn stall_is_real(silent_ms: u64, active: bool, cpu_ms: Option<u64>) -> bool {
    active
        || silent_ms >= BACKGROUND_LONG.as_millis() as u64
        || cpu_ms.is_some_and(|cpu| cpu.saturating_mul(2) > silent_ms)
}

/// Whether the machine slept between two beats: the wall clock moved at
/// least [`WAKE_GAP`] further than the monotonic clock, which stops in sleep.
pub fn woke(wall_ms: u64, monotonic_ms: u64) -> bool {
    wall_ms.saturating_sub(monotonic_ms) >= WAKE_GAP.as_millis() as u64
}

/// A beat fires once a second when awake, so this is no timer's lateness.
const WAKE_GAP: Duration = Duration::from_secs(30);

/// One message per wake of the machine. A window built inside a gpui test
/// gets a channel that is already closed, as `window_status_wake` does, and
/// nothing is ever sent unless [`start`] ran.
pub fn wake_channel() -> futures::channel::mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    if cfg!(test) {
        return receiver;
    }
    if let Ok(mut listeners) = WAKE_LISTENERS.lock() {
        listeners.push(sender);
    }
    receiver
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
    /// One poll at `now_ms` with the last beat at `beat_ms`. `real` decides,
    /// from the silence so far, whether it is a stall at all
    /// ([`stall_is_real`]); until it says so a silence begins nothing, and a
    /// beat that lands first leaves no trace. Asked only past
    /// [`STALL_AFTER`], so a healthy beat never pays for the question.
    pub fn observe(&mut self, now_ms: u64, beat_ms: u64, real: impl FnOnce(u64) -> bool) -> Vec<Event> {
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
            None if silent > STALL_AFTER.as_millis() as u64 && real(silent) => {
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
/// their own beat, verdict and sink.
pub fn run_loop(
    beat_ms: &AtomicU64,
    clock: impl Fn() -> u64,
    poll: Duration,
    stop: &AtomicBool,
    real: impl Fn(u64) -> bool,
    mut sink: impl FnMut(Event),
) {
    let mut detector = Detector::default();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(poll);
        for event in detector.observe(clock(), beat_ms.load(Ordering::Relaxed), &real) {
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
    #[cfg(target_os = "macos")]
    let cpu_of_main = move || main_cpu_ms(main_port);
    #[cfg(not(target_os = "macos"))]
    let cpu_of_main = || None::<u64>;
    let note_cpu = move || CPU_AT_BEAT_MS.store(cpu_of_main().unwrap_or(NO_CPU_READING), Ordering::Relaxed);
    note_cpu();
    beat(true);
    cx.spawn(async move |cx| {
        let mut wall = SystemTime::now();
        let mut monotonic = Instant::now();
        loop {
            cx.background_executor().timer(BEAT_EVERY).await;
            let active = cx.update(|cx| cx.active_window().is_some());
            // The CPU first: a poll that sees this beat must not read the
            // previous beat's CPU against it.
            note_cpu();
            beat(active);
            let (wall_now, monotonic_now) = (SystemTime::now(), Instant::now());
            let wall_ms = wall_now.duration_since(wall).map(|gap| gap.as_millis() as u64).unwrap_or(0);
            if woke(wall_ms, monotonic_now.duration_since(monotonic).as_millis() as u64) {
                if let Ok(mut listeners) = WAKE_LISTENERS.lock() {
                    listeners.retain(|listener| listener.unbounded_send(()).is_ok());
                }
            }
            (wall, monotonic) = (wall_now, monotonic_now);
        }
    })
    .detach();
    let _ = std::thread::Builder::new().name("baaz-watchdog".into()).spawn(move || {
        static STOP: AtomicBool = AtomicBool::new(false);
        let mut cpu_at_begin: Option<u64> = None;
        let mut last_sample: Option<Instant> = None;
        let mut child: Option<std::process::Child> = None;
        let since_beat = move || {
            let at_beat = CPU_AT_BEAT_MS.load(Ordering::Relaxed);
            let now = cpu_of_main()?;
            (at_beat != NO_CPU_READING).then(|| now.saturating_sub(at_beat))
        };
        let real = move |silent_ms: u64| stall_is_real(silent_ms, APP_ACTIVE.load(Ordering::Relaxed), since_beat());
        run_loop(&LAST_BEAT_MS, now_ms, POLL_EVERY, &STOP, real, |event| {
            let cpu_now = cpu_of_main();
            let active = APP_ACTIVE.load(Ordering::Relaxed);
            if let Some(running) = child.as_mut() {
                if !matches!(running.try_wait(), Ok(None)) {
                    child = None;
                }
            }
            match event {
                Event::Began { silent_ms } => {
                    cpu_at_begin = cpu_now;
                    crate::log::file_line(&stall_line(
                        silent_ms,
                        active,
                        since_beat(),
                        crate::log::last_crumb(),
                        &crate::log::last_trace_mark(),
                    ));
                }
                Event::Sample { silent_ms } => {
                    let due = last_sample.is_none_or(|at| at.elapsed() >= SAMPLE_COOLDOWN);
                    // Only a stall that passed `stall_is_real` gets here, but
                    // one reported for its length alone is not worth a sample:
                    // in the background, sample a main thread still at work.
                    let busy = since_beat().is_some_and(|cpu| cpu.saturating_mul(2) > silent_ms);
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
        let real = |_| true;
        assert!(d.observe(100, 50, real).is_empty(), "a fresh beat is no stall");
        assert!(d.observe(1_500, 50, real).is_empty(), "1,450 ms is under the threshold");
        assert_eq!(d.observe(1_600, 50, real), vec![Event::Began { silent_ms: 1_550 }]);
        assert!(d.observe(1_900, 50, real).is_empty(), "a stall is reported once");
        assert_eq!(d.observe(2_100, 50, real), vec![Event::Sample { silent_ms: 2_050 }]);
        assert!(d.observe(3_000, 50, real).is_empty(), "sampled once per stall");
        assert_eq!(d.observe(3_100, 3_050, real), vec![Event::Ended { silent_ms: 2_950 }]);
        assert!(d.observe(3_200, 3_150, real).is_empty(), "beating again is quiet");
    }

    /// What the owner's log held 434 of (2026-10-31..11-04): background,
    /// main-thread CPU 0–3 ms, 1.6–2.1 s. macOS napping a timer, not a stall.
    #[test]
    fn a_coalesced_background_timer_is_not_a_stall() {
        for (silent, cpu) in [(1_600, 0), (1_850, 3), (2_100, 1), (29_999, 40)] {
            assert!(!stall_is_real(silent, false, Some(cpu)), "{silent} ms silent, {cpu} ms CPU");
        }
        assert!(!stall_is_real(2_100, false, None), "no reading is no evidence of work");
        let mut d = Detector::default();
        let real = |silent| stall_is_real(silent, false, Some(2));
        let mut seen = Vec::new();
        for now in (0..=2_100).step_by(250) {
            seen.extend(d.observe(now, 0, real));
        }
        seen.extend(d.observe(2_350, 2_300, real));
        assert_eq!(seen, Vec::new(), "no line at all, not even an `ended`");
    }

    /// A background main thread that is truly stuck still reports, late
    /// enough that the CPU it burned can say so; so does a very long silence
    /// on an idle one; and a frontmost stall reports as it always did.
    #[test]
    fn a_real_stall_reports_in_the_background_and_every_stall_in_front() {
        assert!(stall_is_real(2_000, false, Some(1_900)), "busy for most of it");
        assert!(!stall_is_real(2_000, false, Some(1_000)), "half is not most");
        assert!(stall_is_real(30_000, false, Some(0)), "very long, however idle");
        assert!(stall_is_real(30_000, false, None), "very long with no reading");
        assert!(stall_is_real(1_600, true, Some(0)), "frontmost, blocked on nothing visible");
        assert!(stall_is_real(1_600, true, None));

        let mut d = Detector::default();
        // Idle at first, then the main thread is seen burning CPU.
        assert!(d.observe(1_600, 0, |silent| stall_is_real(silent, false, Some(50))).is_empty());
        assert_eq!(
            d.observe(1_850, 0, |silent| stall_is_real(silent, false, Some(1_800))),
            vec![Event::Began { silent_ms: 1_850 }]
        );
        assert_eq!(d.observe(1_950, 1_900, |_| false), vec![Event::Ended { silent_ms: 1_850 }]);
    }

    /// Asked only past the threshold, so a healthy beat costs no `thread_info`.
    #[test]
    fn a_healthy_beat_never_asks_the_cpu() {
        let mut d = Detector::default();
        d.observe(1_500, 50, |_| panic!("under the threshold"));
    }

    /// The wall clock runs through a sleep, the monotonic one does not.
    #[test]
    fn a_wake_is_the_wall_clock_outrunning_the_monotonic_one() {
        assert!(woke(8 * 3_600_000, 1_000), "slept eight hours between beats");
        assert!(woke(31_000, 1_000));
        assert!(!woke(1_000, 1_000), "an ordinary beat");
        assert!(!woke(1_900, 1_000), "a late timer");
        assert!(!woke(0, 1_000), "the clock moved back");
    }

    /// The thread body on a real clock: beat, go quiet for 1.7 s, beat
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
                run_loop(&beat, move || origin.elapsed().as_millis() as u64, Duration::from_millis(20), &stop, |_| true, |e| {
                    events.lock().unwrap().push(e)
                })
            })
        };
        // Alive for a while.
        for _ in 0..5 {
            beat.store(origin.elapsed().as_millis() as u64, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(30));
        }
        // The simulated stall: no beat for 1.7 s (past STALL_AFTER).
        std::thread::sleep(Duration::from_millis(1_700));
        beat.store(origin.elapsed().as_millis() as u64, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(80));
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        let seen = events.lock().unwrap().clone();
        assert!(matches!(seen.first(), Some(Event::Began { silent_ms }) if *silent_ms > 1_500), "{seen:?}");
        assert!(matches!(seen.last(), Some(Event::Ended { silent_ms }) if *silent_ms >= 1_500), "{seen:?}");
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
