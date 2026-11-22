//! `--screenshot`: render the window off-screen once it has settled,
//! downsample to logical pixels and quit.
//!
//! Lifted from the gallery's own capture path (`aui-gallery/src/shot.rs`) so a
//! baaz PNG and a gallery PNG are the same kind of image: 1×, no window
//! chrome, no pointer.

use std::{path::PathBuf, time::Duration};

use gpui::{App, AsyncApp, WindowHandle};
use gpui_kit::component::Root;

use crate::runtime::CaptureToken;

/// How long a capture that is waiting for an approval will wait.
const APPROVAL_CEILING: Duration = Duration::from_secs(15);
/// How long a capture waits for the app to become able to execute a
/// `--steps`/`--login-steps` list at all — the wire connected and the boot
/// session decided, or the login screen up — before [`STEPS_START_CEILING`]
/// even begins. A hung round-trip behind boot (a `session/list` that never
/// answers) used to sit inside the start ceiling looking exactly like "no
/// script was given": the capture took its screenshot of an empty shell and
/// quit 0, which read as a successful scripted run that had sent nothing.
/// Now the wait for readiness is its own bounded ceiling with its own loud
/// line, and the start ceiling below guards a real race — drain to task
/// start, normally a poll or two — rather than masking a boot that never
/// became ready.
const STEPS_READY_CEILING: Duration = Duration::from_secs(30);
/// How long a capture waits for the `--steps`/`--login-steps` task to even
/// begin, once the app is ready to run it. It is spawned from the same frame
/// that opens the window, so this is normally a poll or two; the ceiling
/// only guards a race, never a script.
const STEPS_START_CEILING: Duration = Duration::from_secs(5);
/// How long a capture waits for a `--steps` script to run to completion,
/// once it has started — deliberately generous, and never shared with
/// [`APPROVAL_CEILING`]: a script's own `wait:` steps routinely add up to far
/// more than 15 s (a scripted send-then-wait-then-send easily clears a
/// minute), and a capture that gave up on the steps list at 15 s used to take
/// the screenshot mid-script and then quit under a turn the later steps
/// never got to send or wait out (see [`capture_and_quit`]'s doc). Bounded
/// only as a backstop against a script that genuinely never finishes.
const STEPS_CEILING: Duration = Duration::from_secs(600);
/// How long a capture waits, after every step and the settling delay have
/// run, for no turn to be running in any session this window has open,
/// before it quits. Quitting mid-turn kills the `muse` child that turn is
/// running on and orphans it (`docs/CONTRIBUTING.md`'s scripting notes).
const TURN_CEILING: Duration = Duration::from_secs(120);
/// How often any of the above looks.
const POLL: Duration = Duration::from_millis(100);

/// Sleep `delay` as a loop of [`POLL`] timers so the foreground executor
/// keeps draining background completions while the deadline runs down.
/// See [`capture_and_quit`].
async fn settle(cx: &gpui::AsyncApp, delay: Duration) {
    let deadline = std::time::Instant::now() + delay;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        cx.background_executor().timer((deadline - now).min(POLL)).await;
    }
}

/// Whether a capture keeps the display's own pixels instead of being
/// downsampled to logical ones: `BAAZ_SHOT_NATIVE=1`.
///
/// The regression suite compares captures byte for byte, and logical pixels
/// are what make two machines agree, so that stays the default. Artwork for
/// the README wants every pixel the retina panel drew, which is what this
/// is for.
pub fn native_pixels() -> bool {
    std::env::var("BAAZ_SHOT_NATIVE").is_ok_and(|v| v == "1")
}

/// Downsample to logical pixels, encode and write, off the UI thread.
///
/// Split out of [`capture_and_quit`] because all three used to run inside the
/// `cx.update` that rendered the window, blocking the UI thread for the
/// length of a Lanczos3 resize plus a PNG encode (finding `performance-15`).
fn write_capture(
    image: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    scale: f32,
    path: &std::path::Path,
) -> anyhow::Result<(u32, u32)> {
    let (w, h) = (image.width(), image.height());
    let scale = if native_pixels() { 1.0 } else { scale };
    let target_w = (w as f32 / scale).round() as u32;
    let target_h = (h as f32 / scale).round() as u32;
    let image = if (target_w, target_h) != (w, h) {
        image::imageops::resize(&image, target_w, target_h, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    image.save(path)?;
    Ok((target_w, target_h))
}

/// Open, move or close the floats for what the main window shows now,
/// without waiting for a frame that a covered window never draws.
fn sync_floats_now(main: WindowHandle<Root>, cx: &mut App) {
    let _ = main.update(cx, |root, window, cx| {
        if let Ok(harness) = root.view().clone().downcast::<crate::app::Harness>() {
            harness.update(cx, |harness, cx| harness.sync_floats(window, cx));
        }
    });
}

type Rgba = image::ImageBuffer<image::Rgba<u8>, Vec<u8>>;

/// Every other window of this app, drawn now and rendered, with its
/// bounds: the overlay windows a capture lays over the main image
/// ([`composite`]). A window that cannot render is left out.
fn overlay_images(main: WindowHandle<Root>, cx: &mut App) -> Vec<(Rgba, gpui::Bounds<gpui::Pixels>)> {
    let main_id = main.window_id();
    let mut out = Vec::new();
    for any in cx.windows() {
        if any.window_id() == main_id {
            continue;
        }
        let _ = gpui::AppContext::update_window(cx, any, |_, window, cx| window.draw(cx).clear(cx));
        if let Ok(Ok(shot)) = any.update(cx, |_, window, _| window.render_to_image().map(|image| (image, window.bounds()))) {
            out.push(shot);
        }
    }
    out
}

/// `main` with each of `floats` alpha-blended over it at its offset from
/// `bounds` (the main window's), in device pixels at `scale`. A float
/// wholly outside the main window changes nothing.
fn composite(mut main: Rgba, scale: f32, bounds: gpui::Bounds<gpui::Pixels>, floats: Vec<(Rgba, gpui::Bounds<gpui::Pixels>)>) -> Rgba {
    for (image, at) in floats {
        let x = ((f32::from(at.origin.x) - f32::from(bounds.origin.x)) * scale).round() as i64;
        let y = ((f32::from(at.origin.y) - f32::from(bounds.origin.y)) * scale).round() as i64;
        image::imageops::overlay(&mut main, &image, x, y);
    }
    main
}

/// A scripted run's own steps do not promise the turn they started
/// is finished — a trailing `send:` with no matching `wait:` after
/// it, or the steps ceiling above giving up early, both leave one in
/// flight — and quitting under a running turn kills its `muse` child
/// and orphans it (the turn resumes as "orphaned" the next time its
/// session opens). Wait, bounded, for every open session's own turn
/// to clear before this process does that.
async fn wait_for_turns(handle: WindowHandle<Root>, cx: &AsyncApp) {
    let deadline = std::time::Instant::now() + TURN_CEILING;
    let mut logged = false;
    while any_turn_running(handle, cx) {
        if std::time::Instant::now() >= deadline {
            eprintln!("baaz: a turn is still running after {TURN_CEILING:?}; quitting anyway");
            break;
        }
        if !logged {
            crate::baaz_log!("waiting for a running turn before quitting a screenshot");
            logged = true;
        }
        cx.background_executor().timer(POLL).await;
    }
}

/// Whether any session `handle`'s window has open has a turn in flight —
/// [`crate::app::Harness::any_turn_running`], reached through the window's
/// root view rather than a per-frame flag, so this reads the state fresh on
/// every poll instead of whatever a stale last render happened to stamp
/// (the render that would stamp it may be many seconds in the past by the
/// time a long `wait:` step ends). `false` on any failure to reach it (the
/// window closed, the root view is not a `Harness`) — the same
/// fail-open-to-"nothing running" this module uses everywhere else a
/// missing signal must not hang a capture forever.
fn any_turn_running(handle: WindowHandle<Root>, cx: &AsyncApp) -> bool {
    cx.update(|cx| {
        handle
            .update(cx, |root, _window, cx| {
                root.view()
                    .clone()
                    .downcast::<crate::app::Harness>()
                    .ok()
                    .is_some_and(|baaz| baaz.read(cx).any_turn_running(cx))
            })
            .unwrap_or(false)
    })
}

/// What a capture waits for before it takes the shot and quits.
#[derive(Clone, Copy, Debug)]
pub struct CaptureWaits {
    /// `--steps` or `--login-steps` were given.
    pub steps: bool,
    /// One of them is a `shell:`, which raises a real, server-minted
    /// approval over a live wire.
    pub approval: bool,
    /// An agent child runs behind the window (anything but `--replay`), so
    /// quitting under a running turn would orphan it.
    pub turns: bool,
}

/// Waits for every scripted step, captures the window and exits the process.
///
/// The order matters, and used to be wrong: a capture used to apply the
/// settling `delay` *before* checking on the steps at all, then gave the
/// steps only [`APPROVAL_CEILING`] (15 s, meant for an approval card) to
/// finish — so a script whose own `wait:` steps added up to more than that
/// (routine: a `send:` needs a `wait:` many times that long) had its
/// screenshot taken mid-script, before steps after the point it gave up had
/// run at all, and then quit the app under whatever turn was still in
/// flight, orphaning it. Now: every step runs to completion first
/// ([`STEPS_CEILING`], a generous backstop rather than a real deadline),
/// then any awaited approval, then the settling `delay` — once, last, never
/// racing the steps it is meant to let settle — and only then, bounded by
/// [`TURN_CEILING`], a wait for no turn to be running anywhere in the window
/// before the process quits and takes the `muse` child with it. A
/// `--replay` run has no child to take, and no frame will ever end its
/// turn, so [`CaptureWaits::turns`] false skips that wait (it used to hold
/// every replay that ends mid-turn for the whole ceiling).
///
/// Every wait here is a loop of [`POLL`] timers, never one `timer(delay)`:
/// in a headless capture nothing else wakes the foreground executor, so a
/// single long sleep starves the background continuations it is waiting for —
/// `--screenshot` without `--steps` never polled the `probe_account`
/// continuation, `account/read`'s answer sat unapplied, and `--login-steps`
/// never ran. The 100 ms wake-ups keep the executor draining completions
/// while the deadline runs down.
pub fn capture_and_quit(
    handle: WindowHandle<Root>,
    path: PathBuf,
    delay: Duration,
    waits: CaptureWaits,
    capture: CaptureToken,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        if waits.steps {
            // Readiness first: the app raises it when it can execute the
            // list at all. Only then does the start ceiling begin, so a
            // boot that never becomes ready cannot look like a script that
            // already finished.
            let ready_deadline = std::time::Instant::now() + STEPS_READY_CEILING;
            while !capture.steps_ready() && std::time::Instant::now() < ready_deadline {
                cx.background_executor().timer(POLL).await;
            }
            if !capture.steps_ready() {
                eprintln!(
                    "baaz: steps never became ready in {STEPS_READY_CEILING:?} (no boot session); capturing anyway"
                );
            } else {
                let start_deadline = std::time::Instant::now() + STEPS_START_CEILING;
                // The flag is only raised once the steps task is scheduled, so
                // this first wait is for "the steps have started", not "the
                // steps are not running yet" — normally a poll or two. A
                // script fast enough to finish between two polls still lands
                // `steps_done`, which is what tells it apart from a drain
                // that never happened.
                while !capture.steps_running()
                    && !capture.steps_done()
                    && std::time::Instant::now() < start_deadline
                {
                    cx.background_executor().timer(POLL).await;
                }
                if !capture.steps_running() && !capture.steps_done() {
                    eprintln!(
                        "baaz: steps never started in {STEPS_START_CEILING:?} after becoming ready; capturing anyway"
                    );
                }
                let steps_deadline = std::time::Instant::now() + STEPS_CEILING;
                while capture.steps_running() && std::time::Instant::now() < steps_deadline {
                    cx.background_executor().timer(POLL).await;
                }
                if capture.steps_running() {
                    eprintln!("baaz: steps still running after {STEPS_CEILING:?}; capturing anyway");
                }
            }
        }
        if waits.approval {
            let deadline = std::time::Instant::now() + APPROVAL_CEILING;
            while !capture.pending_approval() && std::time::Instant::now() < deadline {
                cx.background_executor().timer(POLL).await;
            }
            if !capture.pending_approval() {
                eprintln!("baaz: no approval arrived in {APPROVAL_CEILING:?}; capturing anyway");
            }
        }
        // The settling delay runs last, after every step (including every
        // `wait:`) and any awaited approval have had their turn: whatever
        // the script left on screen, or an arriving card, gets this to
        // animate into before the capture — never a race against steps
        // still running (see this function's own doc).
        settle(cx, delay).await;
        // `BAAZ_SHOT_MIDTURN=1` captures the frame while the turn is still
        // running — the only way to see the first frames after a send — and
        // waits for the turn after the write instead of before it.
        let midturn = std::env::var_os("BAAZ_SHOT_MIDTURN").is_some();
        if !midturn && waits.turns {
            wait_for_turns(handle, cx).await;
        }
        // Only the render stays on the UI thread; the Lanczos3 downsample,
        // the PNG encode and the write go to the background executor
        // (finding `performance-15`). They are the expensive two thirds of a
        // capture and none of them needs the window.
        // The turn-wait above can observe the settled model in the same
        // tick as the finish lands, before the window paints the settle —
        // and `render_to_image` below reads the last painted frame without
        // painting first. Refresh and let one frame land, so the capture
        // shows the settled window rather than the frame from while the
        // turn was still running (W8c). A turn that never settles still
        // waits above on `TURN_CEILING`, never here.
        cx.update(|cx| {
            let _ = handle.update(cx, |_root, window, _cx| {
                window.refresh();
            });
        });
        // The floats (Browse's menus, the Field, the Ask line, toasts) open
        // from `queue_floats`, an `on_next_frame` callback — and gpui stops a
        // window's frames while it is covered or off screen, so a capture
        // whose window sat under another app's never opened them (Gate 8's
        // baseline run: browse-more-menu, -omnibox, -ask-line came out bare).
        // Sync them here, then give their open tasks the wait below.
        cx.update(|cx| sync_floats_now(handle, cx));
        cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
        // gpui-pre 0.3.12 stops a window's frame source while it is not
        // visibly on screen (covered, on another Space, display asleep), so
        // the last painted frame can be the first one. Draw a few frames
        // here — through `update_window`, which leases no view — with a
        // pause between for the work each frame schedules: lists that scroll
        // to their tail, menus placed after a first paint and blocks sized
        // from the previous frame need a second or third frame to settle.
        for _ in 0..4 {
            cx.update(|cx| {
                let _ = gpui::AppContext::update_window(cx, handle.into(), |_, window, cx| window.draw(cx).clear(cx));
            });
            cx.background_executor().timer(std::time::Duration::from_millis(60)).await;
        }
        // Once more after those frames: a float placed from a laid-out rect
        // (the Page float with the Ask line reads the page's rect) has one now.
        cx.update(|cx| sync_floats_now(handle, cx));
        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        let rendered = cx.update(|cx| {
            handle.update(cx, |_root, window, _cx| {
                let scale = window.scale_factor();
                let image = window.render_to_image()?;
                anyhow::Ok((image, scale, window.bounds()))
            })
        });
        // The overlay windows (Browse's menus, the Field, the Ask line, the
        // agent cursor) draw above the page in windows of their own, which
        // a render of the main window alone never showed: four entries
        // captured the shell with no menu (Gate 8). Each window that stands
        // now is live (overlay.rs removes one the moment it is unwanted), so
        // draw it and lay it over the main image at its offset.
        let floats = cx.update(|cx| overlay_images(handle, cx));
        let result = match rendered {
            Ok(Ok((image, scale, bounds))) => {
                let path = path.clone();
                cx.background_executor()
                    .spawn(async move { write_capture(composite(image, scale, bounds, floats), scale, &path) })
                    .await
            }
            Ok(Err(err)) | Err(err) => Err(err),
        };
        match result {
            Ok((w, h)) => println!("wrote {} ({w}\u{d7}{h})", path.display()),
            Err(err) => eprintln!("screenshot failed: {err:#}"),
        }
        if midturn && waits.turns {
            wait_for_turns(handle, cx).await;
        }
        // The billing probe may still be driving the `muse` TUI on a
        // background thread, and quitting now would orphan it: the child is
        // its own session leader, so it is reparented to pid 1 and the
        // `Pty` drop that would SIGKILL it never runs. Kill the live probe
        // first — the thread then finishes against a dead child and reaps it
        // on drop — and wait, bounded, for that drop, so the pid file is
        // gone too and no `muse` process outlives this quit.
        crate::tier::cleanup_probes();
        // A scripted run fails when any step did. Steps that ran were
        // already counted by the runners; whatever list is still undrained
        // here never became runnable at all (no open session to run it
        // against), so classify it now — free table lookups, nothing
        // reaches the wire. One line per failure naming the step, then the
        // machine-readable summary, then a non-zero exit. Unscripted
        // captures print nothing new.
        if waits.steps {
            let leftover: Vec<String> = cx.update(|cx| {
                handle
                    .update(cx, |root, _window, cx| {
                        root.view()
                            .clone()
                            .downcast::<crate::app::Harness>()
                            .ok()
                            .map(|baaz| {
                                let harness = baaz.read(cx);
                                harness
                                    .args
                                    .steps
                                    .iter()
                                    .chain(harness.args.login_steps.iter())
                                    .cloned()
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .unwrap_or_default()
            });
            crate::steps::record_unrun_steps(&leftover);
            for name in crate::steps::step_failure_names() {
                println!("steps: failed step `{name}`");
            }
            println!(
                "steps: ran={} failed={}",
                crate::steps::steps_ran(),
                crate::steps::step_failures()
            );
        }
        if crate::steps::step_failures() > 0 {
            let _ = std::io::Write::flush(&mut std::io::stdout());
            // `exit` skips `on_app_quit`: without this Chromium's clone and the
            // capture's state dir stay behind (docs/74 §1).
            #[cfg(feature = "cef")]
            aui_cef::shutdown(std::time::Duration::from_secs(3));
            crate::store::remove_capture_state();
            std::process::exit(1);
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;

    /// Pins the actual bug: the steps wait used to share `APPROVAL_CEILING`
    /// (15 s), and a script's own `wait:` steps routinely add up to far more
    /// than that, so the capture gave up on the steps list — and took the
    /// screenshot, and later quit the app — while steps (and the turn they
    /// started) were still running. `STEPS_CEILING` must never collapse back
    /// onto `APPROVAL_CEILING`.
    #[test]
    fn the_steps_ceiling_is_not_the_approval_ceiling() {
        assert!(STEPS_CEILING > APPROVAL_CEILING);
    }
}
