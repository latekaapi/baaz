//! `--bench`: streaming replay plus scroll plus whole-frame timing.
//!
//! The old `bench:<n>` step only re-notified frames on a static replay, so it
//! could not measure streaming fold cost, scroll cost, or layout/paint. This
//! mode streams a capture's `<--` lines through the fold on a timer at a
//! fixed cadence (so streaming cost is real) while driving the transcript
//! `ListState` programmatically, and measures element-construction time (the
//! existing `render_transcript` timer), whole-frame intervals between the
//! paints the stream and the scroll drive, fold-apply time per event, and
//! peak RSS. Every frame counted is one the app requested itself — nothing
//! here forces demand — so the numbers hold wherever frames render at all,
//! including a display-asleep machine where a chained frame callback would
//! never fire.
//!
//! Free: no child, no server — a replayed session refuses every command with
//! a banner, and this mode never issues one.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, Window, WindowHandle};
use gpui_kit::component::Root;

use crate::session::{self, SessionView};
use muse_client::MuseEvent;

/// How the bench drives the transcript list while the stream lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BenchScroll {
    /// Pinned to the first turn.
    Top,
    /// Pinned to the middle, re-centred as the list grows.
    Mid,
    /// Tail-follow, the reader's usual position.
    Tail,
    /// Top → tail → top over the run.
    #[default]
    Sweep,
}

impl BenchScroll {
    /// Parse `--bench-scroll top|mid|tail|sweep`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top" => Some(Self::Top),
            "mid" => Some(Self::Mid),
            "tail" => Some(Self::Tail),
            "sweep" => Some(Self::Sweep),
            _ => None,
        }
    }
}

/// What one `--bench` run does.
#[derive(Clone, Debug)]
pub struct BenchOptions {
    /// The capture to stream.
    pub capture: PathBuf,
    /// Delay between streamed events.
    pub cadence: Duration,
    /// How the list is driven.
    pub scroll: BenchScroll,
    /// Minimum frames to observe before stopping.
    pub frames: usize,
    /// Where to write the JSON row, if any.
    pub out: Option<PathBuf>,
    /// Leave the capture's last turn running: the stream stops before its
    /// `turn/completed`, so the idle window is measured against a transcript
    /// that is still streaming rather than a settled one (finding
    /// `performance-13`). A settled transcript must request no frames at all;
    /// an open turn must request the frames its elapsed row needs — one a
    /// second — and no more.
    pub open_turn: bool,
}

/// The events to stream, cut before the last `turn/completed` when the run
/// asked for an open turn.
///
/// The cut is what leaves the view "Working…" with a live ticker: the 1 Hz
/// elapsed row is the only clock that should still be asking for frames, so
/// `bench-idle` under this flag is that clock's own count and anything above
/// it is a clock that failed to gate itself.
fn cut_for_open_turn(events: Vec<MuseEvent>, open_turn: bool) -> Vec<MuseEvent> {
    if !open_turn {
        return events;
    }
    let last_completed = events.iter().rposition(|event| {
        matches!(event, MuseEvent::Notification { method, .. } if method == "turn/completed")
    });
    match last_completed {
        Some(at) => events.into_iter().take(at).collect(),
        None => events,
    }
}

/// The bench window's root: one replayed session view, rendered whole every
/// frame so element construction, layout and paint are all real.
pub struct BenchRoot {
    pub(crate) view: Entity<SessionView>,
}

impl BenchRoot {
    /// Open the session view a bench run streams into. The driver assigns
    /// the capture's session id and sent prompts before the first event
    /// (see [`run`]): the view starts on a placeholder.
    pub fn new(workspace: String, provider: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = crate::session::SessionHost {
            provider_id: provider,
            workspace,
            overlays: cx.new(|_| crate::overlays::Overlays::default()),
            capture: crate::shot::CaptureToken::default(),
        };
        let view = cx.new(|cx| SessionView::new("bench".to_owned(), None, host, window, cx));
        Self { view }
    }
}

impl Render for BenchRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view.update(cx, |view, cx| view.render_centre(window, cx))
    }
}

/// Frames pending past 16.7 ms count as dropped.
const FRAME_BUDGET: Duration = Duration::from_micros(16_700);
/// How long the idle assertion watches a settled transcript.
const IDLE_WINDOW: Duration = Duration::from_secs(2);
/// How often the driver polls for frame counts.
const POLL: Duration = Duration::from_millis(50);

/// `p50`/`p90`/… over a sorted sample: the element at `q * len`, clamped.
fn percentile(sorted: &[u128], q: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((q * sorted.len() as f64) as usize).min(sorted.len() - 1)]
}

/// The sweep position after feeding event `index` of `total`: top → tail →
/// top over the run.
fn sweep_frac(index: usize, total: usize) -> f32 {
    if total <= 1 {
        return 1.0;
    }
    let t = index as f32 / (total - 1) as f32;
    1.0 - (2.0 * t - 1.0).abs()
}

/// Peak resident set size of this process, in bytes.
fn peak_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0;
    if !ok {
        return 0;
    }
    let usage = unsafe { usage.assume_init() };
    #[cfg(target_os = "macos")]
    {
        usage.ru_maxrss as u64
    }
    #[cfg(not(target_os = "macos"))]
    {
        (usage.ru_maxrss as u64) * 1024
    }
}

/// The git short hash of this checkout, for the JSON row.
fn git_hash() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|hash| !hash.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Drive the bench: stream the capture, scroll the list, then print one line
/// per metric and quit.
pub fn run(handle: WindowHandle<Root>, root: Entity<BenchRoot>, opts: BenchOptions, command: String, cx: &mut App) {
    let (events, sent) = match session::parse_replay_file(&opts.capture) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("bench: {}: {error}", opts.capture.display());
            cx.quit();
            return;
        }
    };
    // `SessionView::apply` drops events for any other session, so the view
    // opens on the capture's own id (a capture names exactly one) and learns
    // the prompts turns were sent with, exactly like `--replay`.
    let session_id = events
        .iter()
        .find_map(|event| match event {
            MuseEvent::Notification { session_id: Some(id), .. } => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "bench".to_owned());
    let events = cut_for_open_turn(events, opts.open_turn);
    root.update(cx, |root, cx| {
        root.view.update(cx, |view, cx| view.begin_bench_replay(session_id, sent, cx));
    });
    let view = root.read(cx).view.clone();
    cx.spawn(async move |cx| {
        // Held to the end: dropping the window handle closes the window.
        let _window = handle;
        let total = events.len();
        let start_samples = session::frame_sample_count();
        let run_start = Instant::now();
        // The stream: one fold-apply per cadence tick, so streaming cost is
        // real, with the list driven per the scroll mode on the same update.
        let mut apply_us: Vec<u128> = Vec::with_capacity(total);
        if opts.scroll == BenchScroll::Top {
            cx.update(|cx| view.update(cx, |view, cx| view.bench_scroll_to(0.0, cx)));
        }
        for (index, event) in events.into_iter().enumerate() {
            let frac = sweep_frac(index, total);
            let micros: u128 = cx.update(|cx| {
                view.update(cx, |view, cx| {
                    let start = Instant::now();
                    view.apply(event, cx);
                    let micros = start.elapsed().as_micros();
                    match opts.scroll {
                        BenchScroll::Top => {}
                        BenchScroll::Mid => view.bench_scroll_to(0.5, cx),
                        BenchScroll::Tail => view.bench_scroll_tail(cx),
                        BenchScroll::Sweep => view.bench_scroll_to(frac, cx),
                    }
                    micros
                })
            });
            apply_us.push(micros);
            cx.background_executor().timer(opts.cadence).await;
        }
        // The frames: a small capture streams in milliseconds, so keep
        // sweeping until the asked frame count renders. Every scroll update
        // notifies, so every pass earns natural frames — nothing here forces
        // demand the app did not ask for. Bounded, so a frame-less
        // environment still reports what it saw.
        let mut extra = 0usize;
        while session::frame_sample_count() < start_samples + opts.frames as u64
            && extra < opts.frames * 2 + 200
        {
            extra += 1;
            let frac = sweep_frac(extra % 200, 200);
            cx.update(|cx| view.update(cx, |view, cx| view.bench_scroll_to(frac, cx)));
            cx.background_executor().timer(opts.cadence).await;
        }
        let stream_secs = run_start.elapsed().as_secs_f64();
        // The idle assertion: a settled transcript must request no frames.
        // `render_transcript` records one sample per construction, so the
        // sample delta over a quiet 2 s is the frame count there.
        let idle_start = session::frame_sample_count();
        let deadline = Instant::now() + IDLE_WINDOW;
        while Instant::now() < deadline {
            cx.background_executor().timer(POLL).await;
        }
        let idle_frames = session::frame_sample_count().saturating_sub(idle_start);
        // The tables.
        let mut element = session::take_frame_samples();
        element.sort_unstable();
        apply_us.sort_unstable();
        let times = session::take_frame_times();
        let mut gaps_ms: Vec<u128> = times
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]).as_micros() / 1000)
            .collect();
        gaps_ms.sort_unstable();
        let dropped = times
            .windows(2)
            .filter(|pair| pair[1].duration_since(pair[0]) > FRAME_BUDGET)
            .count();
        let frames = times.len() as u64;
        let span_secs = match (times.first(), times.last()) {
            (Some(first), Some(last)) => last.duration_since(*first).as_secs_f64(),
            _ => 0.0,
        };
        let fps = if span_secs > 0.0 { frames as f64 / span_secs } else { 0.0 };
        let rss_mb = peak_rss_bytes() as f64 / 1_048_576.0;
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        println!(
            "bench-element n={} p50={}us p90={}us p99={}us max={}us",
            element.len(),
            percentile(&element, 0.5),
            percentile(&element, 0.9),
            percentile(&element, 0.99),
            element.last().copied().unwrap_or(0)
        );
        println!(
            "bench-apply n={} p50={}us p90={}us p99={}us max={}us",
            apply_us.len(),
            percentile(&apply_us, 0.5),
            percentile(&apply_us, 0.9),
            percentile(&apply_us, 0.99),
            apply_us.last().copied().unwrap_or(0)
        );
        println!(
            "bench-frame n={} p50={}ms p90={}ms p99={}ms max={}ms",
            gaps_ms.len(),
            percentile(&gaps_ms, 0.5),
            percentile(&gaps_ms, 0.9),
            percentile(&gaps_ms, 0.99),
            gaps_ms.last().copied().unwrap_or(0)
        );
        println!("bench-frames frames={frames} fps={fps:.1} dropped={dropped} stream_secs={stream_secs:.1}");
        println!("bench-rss peak_mb={rss_mb:.1}");
        println!("bench-idle frames_2s={idle_frames} open_turn={}", opts.open_turn);
        if let Some(path) = opts.out.as_ref() {
            let row = serde_json::json!({
                "command": command,
                "capture": opts.capture.display().to_string(),
                "profile": profile,
                "git": git_hash(),
                "element": {
                    "n": element.len(),
                    "p50_us": percentile(&element, 0.5),
                    "p90_us": percentile(&element, 0.9),
                    "p99_us": percentile(&element, 0.99),
                    "max_us": element.last().copied().unwrap_or(0),
                },
                "fold_apply": {
                    "n": apply_us.len(),
                    "p50_us": percentile(&apply_us, 0.5),
                    "p90_us": percentile(&apply_us, 0.9),
                    "p99_us": percentile(&apply_us, 0.99),
                    "max_us": apply_us.last().copied().unwrap_or(0),
                },
                "frame": {
                    "n": gaps_ms.len(),
                    "p50_ms": percentile(&gaps_ms, 0.5),
                    "p90_ms": percentile(&gaps_ms, 0.9),
                    "p99_ms": percentile(&gaps_ms, 0.99),
                    "max_ms": gaps_ms.last().copied().unwrap_or(0),
                },
                "frames": frames,
                "fps": fps,
                "dropped": dropped,
                "stream_secs": stream_secs,
                "idle_frames_2s": idle_frames,
                "idle_open_turn": opts.open_turn,
                "rss_peak_mb": rss_mb,
            });
            match serde_json::to_string_pretty(&row)
                .map_err(|error| error.to_string())
                .and_then(|text| std::fs::write(path, text).map_err(|error| error.to_string()))
            {
                Ok(()) => println!("bench-out {}", path.display()),
                Err(error) => eprintln!("bench: cannot write {}: {error}", path.display()),
            }
        }
        crate::tier::cleanup_probes();
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_index_into_the_sorted_sample() {
        let sorted = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&sorted, 0.5), 6);
        assert_eq!(percentile(&sorted, 0.9), 10);
        assert_eq!(percentile(&sorted, 0.99), 10);
        assert_eq!(percentile(&sorted, 0.0), 1);
    }

    #[test]
    fn an_empty_sample_reports_zero() {
        assert_eq!(percentile(&[], 0.5), 0);
    }

    #[test]
    fn the_sweep_runs_top_to_tail_to_top() {
        assert_eq!(sweep_frac(0, 101), 0.0);
        assert_eq!(sweep_frac(100, 101), 0.0);
        assert_eq!(sweep_frac(50, 101), 1.0);
    }

    #[test]
    fn every_scroll_mode_parses() {
        assert_eq!(BenchScroll::parse("top"), Some(BenchScroll::Top));
        assert_eq!(BenchScroll::parse("mid"), Some(BenchScroll::Mid));
        assert_eq!(BenchScroll::parse("tail"), Some(BenchScroll::Tail));
        assert_eq!(BenchScroll::parse("sweep"), Some(BenchScroll::Sweep));
        assert_eq!(BenchScroll::parse("sideways"), None);
    }
}
