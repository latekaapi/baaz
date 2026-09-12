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

use gpui::{
    point, px, App, AppContext as _, AsyncApp, Context, Entity, IntoElement, ListOffset,
    PlatformInput, Render, ScrollDelta, ScrollWheelEvent, Window, WindowHandle,
};
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
    /// Real wheel events at the transcript centre, after the stream lands
    /// (the scroll-jank instrument: `sweep` drives `scroll_to` and never
    /// exercises `ListState::scroll`).
    Wheel,
}

impl BenchScroll {
    /// Parse `--bench-scroll top|mid|tail|sweep`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top" => Some(Self::Top),
            "mid" => Some(Self::Mid),
            "tail" => Some(Self::Tail),
            "sweep" => Some(Self::Sweep),
            "wheel" => Some(Self::Wheel),
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

/// The wheel instrument's phases: (name, vertical px per event, event
/// count). Positive climbs toward the head, negative descends toward the
/// tail — gpui subtracts the wheel delta from the pixel scroll top, so a
/// `+120 px` event moves the view up by 120 px.
///
/// (a) is a flick up from the tail; (b) comes back down past it; (c) is a
/// slow trackpad climb and (d) its exact mirror, so (d) must end where (c)
/// started without ever clamping.
pub(crate) const WHEEL_PHASES: [(char, f32, usize); 4] = [
    ('a', 600.0, 6),
    ('b', -40.0, 90),
    ('c', 20.0, 300),
    ('d', -20.0, 300),
];

/// How long one wheel event waits for its frame before giving up on it.
const WHEEL_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// Frames to let the tail settle before the first wheel phase.
const WHEEL_SETTLE_FRAMES: usize = 3;

/// One wheel phase's outcome, for the `bench-scroll` line.
#[derive(Debug, Default)]
struct WheelPhaseStats {
    /// `a`..=`d`, in [`WHEEL_PHASES`] order.
    name: char,
    /// Events dispatched in this phase.
    events: usize,
    /// Frames observed while they landed.
    frames: usize,
    /// Frames where `item_ix` moved across more rows than the event's
    /// travel explains (a teleport).
    jumps: usize,
    /// Frames where an event was dispatched and the position did not move
    /// short of a scroll limit (the jank: mid-list rubber-band stalls).
    stalls: usize,
    /// Frames where the position did not move because the event ran into
    /// the head (`item_ix` 0) or the tail (`is_scrolled_to_end`) limit —
    /// correct end-of-list behaviour, not jank. `stalls + clamped` is every
    /// no-move frame.
    clamped: usize,
    /// `item_ix` when the phase ended.
    end_ix: usize,
}

/// How one wheel event's frame reads: teleported, stalled short of a limit,
/// clamped at one, or moved.
#[derive(Debug, PartialEq, Eq)]
struct WheelStep {
    jumped: bool,
    stalled: bool,
    clamped: bool,
}

/// Rows one wheel event may legitimately cross before the step counts as a
/// jump: the event's travel at the first-fill height hint, plus slack.
fn expected_rows(dy: f32) -> usize {
    (dy.abs() / session::TURN_HEIGHT_HINT).ceil() as usize + 3
}

/// Classify one sampled wheel step. A nonzero pixel step always moves the
/// offset mid-list, so a no-move frame is either pinned at a scroll limit
/// (up against the head, down against the tail) or a genuine stall; a move
/// across more rows than the delta explains is a teleport.
fn classify_wheel_step(dy: f32, pre: ListOffset, post: ListOffset, at_end: bool) -> WheelStep {
    if post.item_ix != pre.item_ix || post.offset_in_item != pre.offset_in_item {
        return WheelStep {
            jumped: post.item_ix.abs_diff(pre.item_ix) > expected_rows(dy),
            stalled: false,
            clamped: false,
        };
    }
    let clamped = if dy > 0.0 {
        post.item_ix == 0 && post.offset_in_item == px(0.0)
    } else {
        at_end
    };
    WheelStep {
        jumped: false,
        stalled: !clamped,
        clamped,
    }
}

/// Wait until `render_transcript` has painted past `since`, so one wheel
/// event earns one sampled frame. Returns the frames seen (0 on timeout —
/// the caller counts the event stalled and moves on).
async fn wait_for_frame(cx: &mut AsyncApp, since: u64) -> usize {
    let deadline = Instant::now() + WHEEL_FRAME_TIMEOUT;
    loop {
        let now = session::frame_sample_count();
        if now > since {
            return (now - since) as usize;
        }
        if Instant::now() >= deadline {
            return 0;
        }
        cx.background_executor()
            .timer(Duration::from_millis(5))
            .await;
    }
}

/// The wheel instrument: pin the tail, then dispatch real `ScrollWheelEvent`s
/// at the window centre — where the transcript is — one per frame, sampling
/// the list's `logical_scroll_top` after each.
///
/// Real events matter because `sweep` drives `scroll_to(ListOffset)` and
/// never touches `ListState::scroll`, the pixel-delta path a person's wheel
/// takes through the sum tree's heights.
async fn drive_wheel(
    handle: &WindowHandle<Root>,
    view: &Entity<SessionView>,
    cx: &mut AsyncApp,
) -> (usize, Vec<WheelPhaseStats>) {
    let top = |cx: &mut AsyncApp| -> (ListOffset, bool) {
        cx.update(|cx| (view.read(cx).bench_list_top(), view.read(cx).bench_list_end()))
    };
    // Pin the tail first — the anchor every phase starts from — then let
    // layout settle so the first phase starts from a realized tail. The
    // pin is unconditional: with zero settle frames the flick lands on the
    // same frame as the jump, before layout measures anything there.
    cx.update(|cx| view.update(cx, |view, cx| view.bench_scroll_tail(cx)));
    let mut tail_ix = top(cx).0.item_ix;
    for _ in 0..WHEEL_SETTLE_FRAMES {
        let since = session::frame_sample_count();
        cx.update(|cx| view.update(cx, |view, cx| view.bench_scroll_tail(cx)));
        wait_for_frame(cx, since).await;
        tail_ix = top(cx).0.item_ix;
    }
    let mut phases = Vec::with_capacity(WHEEL_PHASES.len());
    for (name, dy, count) in WHEEL_PHASES {
        let mut stats = WheelPhaseStats {
            name,
            ..WheelPhaseStats::default()
        };
        for _ in 0..count {
            let before = session::frame_sample_count();
            let pre = top(cx).0;
            let dispatched = handle
                .update(cx, |_, window, cx| {
                    let size = window.bounds().size;
                    window.dispatch_event(
                        PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position: point(size.width * 0.5, size.height * 0.5),
                            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .is_ok();
            if !dispatched {
                break;
            }
            stats.events += 1;
            stats.frames += wait_for_frame(cx, before).await;
            let (post, at_end) = top(cx);
            let step = classify_wheel_step(dy, pre, post, at_end);
            stats.jumps += step.jumped as usize;
            stats.stalls += step.stalled as usize;
            stats.clamped += step.clamped as usize;
            stats.end_ix = post.item_ix;
        }
        phases.push(stats);
    }
    (tail_ix, phases)
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
        // `wheel` streams head-pinned too: an opened session arrives whole
        // and is scrolled to its tail with everything above unmeasured,
        // which is the state the instrument has to start its flick from.
        if matches!(opts.scroll, BenchScroll::Top | BenchScroll::Wheel) {
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
                        // Wheel stays head-pinned through the stream (the
                        // first fill's `follow` would otherwise pull it to
                        // the tail and measure every row on the way) and is
                        // driven by real events after it, below.
                        BenchScroll::Wheel => view.bench_scroll_to(0.0, cx),
                    }
                    micros
                })
            });
            apply_us.push(micros);
            cx.background_executor().timer(opts.cadence).await;
        }
        // The wheel instrument runs after the stream, below, and its phases
        // are the frames — about 700 of them — so it skips this sweep.
        let mut wheel_stats: Option<(usize, Vec<WheelPhaseStats>)> = None;
        if opts.scroll == BenchScroll::Wheel {
            wheel_stats = Some(drive_wheel(&_window, &view, cx).await);
        }
        // The frames: a small capture streams in milliseconds, so keep
        // sweeping until the asked frame count renders. Every scroll update
        // notifies, so every pass earns natural frames — nothing here forces
        // demand the app did not ask for. Bounded, so a frame-less
        // environment still reports what it saw.
        if opts.scroll != BenchScroll::Wheel {
            let mut extra = 0usize;
            while session::frame_sample_count() < start_samples + opts.frames as u64
                && extra < opts.frames * 2 + 200
            {
                extra += 1;
                let frac = sweep_frac(extra % 200, 200);
                cx.update(|cx| view.update(cx, |view, cx| view.bench_scroll_to(frac, cx)));
                cx.background_executor().timer(opts.cadence).await;
            }
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
        let scroll_json = match wheel_stats.as_ref() {
            Some((tail_ix, phases)) => {
                let events: usize = phases.iter().map(|p| p.events).sum();
                let frames: usize = phases.iter().map(|p| p.frames).sum();
                let jumps: usize = phases.iter().map(|p| p.jumps).sum();
                let stalls: usize = phases.iter().map(|p| p.stalls).sum();
                let clamped: usize = phases.iter().map(|p| p.clamped).sum();
                let ix = |i: usize| phases.get(i).map(|p| p.end_ix).unwrap_or(0);
                let jump = |i: usize| phases.get(i).map(|p| p.jumps).unwrap_or(0);
                let stall = |i: usize| phases.get(i).map(|p| p.stalls).unwrap_or(0);
                let clamp = |i: usize| phases.get(i).map(|p| p.clamped).unwrap_or(0);
                println!(
                    "bench-scroll events={events} frames={frames} jumps={jumps} stalls={stalls} clamped={clamped} tail_ix={tail_ix} a_ix={} b_ix={} c_ix={} d_ix={} a_jumps={} a_stalls={} a_clamped={} b_jumps={} b_stalls={} b_clamped={} c_jumps={} c_stalls={} c_clamped={} d_jumps={} d_stalls={} d_clamped={}",
                    ix(0),
                    ix(1),
                    ix(2),
                    ix(3),
                    jump(0),
                    stall(0),
                    clamp(0),
                    jump(1),
                    stall(1),
                    clamp(1),
                    jump(2),
                    stall(2),
                    clamp(2),
                    jump(3),
                    stall(3),
                    clamp(3),
                );
                serde_json::json!({
                    "events": events,
                    "frames": frames,
                    "jumps": jumps,
                    "stalls": stalls,
                    "clamped": clamped,
                    "tail_ix": tail_ix,
                    "phases": phases.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "events": p.events,
                        "frames": p.frames,
                        "jumps": p.jumps,
                        "stalls": p.stalls,
                        "clamped": p.clamped,
                        "end_ix": p.end_ix,
                    })).collect::<Vec<_>>(),
                })
            }
            None => serde_json::Value::Null,
        };
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
                "scroll": scroll_json,
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
        assert_eq!(BenchScroll::parse("wheel"), Some(BenchScroll::Wheel));
        assert_eq!(BenchScroll::parse("sideways"), None);
    }

    #[test]
    fn the_wheel_phases_cover_flick_down_and_trackpad() {
        let events: usize = WHEEL_PHASES.iter().map(|(_, _, count)| count).sum();
        assert_eq!(events, 6 + 90 + 300 + 300);
        // (a) is a real flick: each event travels several rows, which is
        // what exposes zero-height unmeasured rows.
        assert_eq!(WHEEL_PHASES[0], ('a', 600.0, 6));
        // (c) and (d) mirror each other, so a healthy run returns (d) to the
        // tail without ever stalling mid-list.
        let net_c_d: f32 = WHEEL_PHASES[2..]
            .iter()
            .map(|(_, dy, count)| dy * *count as f32)
            .sum();
        assert_eq!(net_c_d, 0.0);
    }

    #[test]
    fn wheel_steps_split_jank_from_end_of_list() {
        use gpui::ListOffset;
        let at = |ix: usize, off: f32| ListOffset {
            item_ix: ix,
            offset_in_item: px(off),
        };
        // A smooth climb is none of the three.
        assert_eq!(
            classify_wheel_step(120.0, at(591, 0.0), at(590, 0.0), false),
            WheelStep {
                jumped: false,
                stalled: false,
                clamped: false,
            }
        );
        // A teleport jumps.
        assert_eq!(
            classify_wheel_step(120.0, at(592, 0.0), at(2, 0.0), false),
            WheelStep {
                jumped: true,
                stalled: false,
                clamped: false,
            }
        );
        // A no-move mid-list stalls.
        assert_eq!(
            classify_wheel_step(-40.0, at(300, 10.0), at(300, 10.0), false),
            WheelStep {
                jumped: false,
                stalled: true,
                clamped: false,
            }
        );
        // A no-move against the head clamps.
        assert_eq!(
            classify_wheel_step(20.0, at(0, 0.0), at(0, 0.0), false),
            WheelStep {
                jumped: false,
                stalled: false,
                clamped: true,
            }
        );
        // A no-move against the tail clamps, even with an offset.
        assert_eq!(
            classify_wheel_step(-20.0, at(590, 4.0), at(590, 4.0), true),
            WheelStep {
                jumped: false,
                stalled: false,
                clamped: true,
            }
        );
        // Unknown tail state (unmeasured, unscrollable) follows the app's
        // own follow logic and counts as the tail limit.
        assert_eq!(
            classify_wheel_step(-20.0, at(0, 0.0), at(0, 0.0), true),
            WheelStep {
                jumped: false,
                stalled: false,
                clamped: true,
            }
        );
    }
}
