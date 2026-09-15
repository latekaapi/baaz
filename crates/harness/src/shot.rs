//! `--screenshot`: render the window off-screen once it has settled,
//! downsample to logical pixels and quit.
//!
//! Lifted from the gallery's own capture path (`aui-gallery/src/shot.rs`) so a
//! harness PNG and a gallery PNG are the same kind of image: 1×, no window
//! chrome, no pointer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::{path::PathBuf, time::Duration};

use gpui::{App, AsyncApp, WindowHandle};
use gpui_kit::component::Root;

/// The two facts a `--screenshot` wait needs out of the window it is capturing.
///
/// A token rather than a callback because the capture runs outside the entity
/// tree: [`capture_and_quit`] holds a `WindowHandle`, not the session. A token
/// rather than a pair of process globals because a second window would then
/// corrupt the first one's wait (finding `support-16`): [`crate::app::Harness`]
/// owns one, hands the same one to every session view it opens, and `main`
/// hands the same one to [`capture_and_quit`], so each window waits on its own
/// flags.
#[derive(Clone, Default)]
pub struct CaptureToken(std::sync::Arc<Flags>);

#[derive(Default)]
pub struct Flags {
    /// Whether the open session is showing a pending approval right now. The
    /// session sets it on every frame it renders.
    pending_approval: AtomicBool,
    /// Whether a `--steps` list is still running.
    ///
    /// A scripted capture used to race its own script: the delay is measured
    /// from the first frame, and a step list containing a `wait:` outlives it,
    /// so the PNG showed the window before the steps that were the point of
    /// taking it.
    steps_running: AtomicBool,
}

impl CaptureToken {
    /// Called by the session view each frame: is a card waiting on the person?
    pub fn set_pending_approval(&self, pending: bool) {
        self.0.pending_approval.store(pending, Ordering::Relaxed);
    }

    fn pending_approval(&self) -> bool {
        self.0.pending_approval.load(Ordering::Relaxed)
    }

    /// Called by the application around its `--steps` and `--login-steps` loops.
    pub fn set_steps_running(&self, running: bool) {
        self.0.steps_running.store(running, Ordering::Relaxed);
    }

    fn steps_running(&self) -> bool {
        self.0.steps_running.load(Ordering::Relaxed)
    }
}

/// How long a capture that is waiting for an approval will wait.
const APPROVAL_CEILING: Duration = Duration::from_secs(15);
/// How long a capture waits for the `--steps`/`--login-steps` task to even
/// begin. It is spawned from the same frame that opens the window, so this
/// is normally a poll or two; the ceiling only guards a race, never a script.
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
                    .is_some_and(|harness| harness.read(cx).any_turn_running(cx))
            })
            .unwrap_or(false)
    })
}

/// Waits for every scripted step, captures the window and exits the process.
///
/// `await_steps` is set whenever `--steps` or `--login-steps` were given, and
/// `await_approval` when one of them is a `shell:`, which raises a real,
/// server-minted approval over a live wire.
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
/// before the process quits and takes the `muse` child with it.
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
    await_steps: bool,
    await_approval: bool,
    capture: CaptureToken,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        if await_steps {
            let start_deadline = std::time::Instant::now() + STEPS_START_CEILING;
            // The flag is only raised once the steps task is scheduled, so
            // this first wait is for "the steps have started", not "the
            // steps are not running yet" — normally a poll or two.
            while !capture.steps_running() && std::time::Instant::now() < start_deadline {
                cx.background_executor().timer(POLL).await;
            }
            let steps_deadline = std::time::Instant::now() + STEPS_CEILING;
            while capture.steps_running() && std::time::Instant::now() < steps_deadline {
                cx.background_executor().timer(POLL).await;
            }
            if capture.steps_running() {
                eprintln!("harness: steps still running after {STEPS_CEILING:?}; capturing anyway");
            }
        }
        if await_approval {
            let deadline = std::time::Instant::now() + APPROVAL_CEILING;
            while !capture.pending_approval() && std::time::Instant::now() < deadline {
                cx.background_executor().timer(POLL).await;
            }
            if !capture.pending_approval() {
                eprintln!("harness: no approval arrived in {APPROVAL_CEILING:?}; capturing anyway");
            }
        }
        // The settling delay runs last, after every step (including every
        // `wait:`) and any awaited approval have had their turn: whatever
        // the script left on screen, or an arriving card, gets this to
        // animate into before the capture — never a race against steps
        // still running (see this function's own doc).
        settle(cx, delay).await;
        // A scripted run's own steps do not promise the turn they started
        // is finished — a trailing `send:` with no matching `wait:` after
        // it, or the steps ceiling above giving up early, both leave one in
        // flight — and quitting under a running turn kills its `muse` child
        // and orphans it (the turn resumes as "orphaned" the next time its
        // session opens). Wait, bounded, for every open session's own turn
        // to clear before this process does that.
        {
            let deadline = std::time::Instant::now() + TURN_CEILING;
            let mut logged = false;
            while any_turn_running(handle, cx) {
                if std::time::Instant::now() >= deadline {
                    eprintln!("harness: a turn is still running after {TURN_CEILING:?}; quitting anyway");
                    break;
                }
                if !logged {
                    crate::harness_log!("waiting for a running turn before quitting a screenshot");
                    logged = true;
                }
                cx.background_executor().timer(POLL).await;
            }
        }
        // Only the render stays on the UI thread; the Lanczos3 downsample,
        // the PNG encode and the write go to the background executor
        // (finding `performance-15`). They are the expensive two thirds of a
        // capture and none of them needs the window.
        let rendered = cx.update(|cx| {
            handle.update(cx, |_root, window, _cx| {
                let scale = window.scale_factor();
                let image = window.render_to_image()?;
                anyhow::Ok((image, scale))
            })
        });
        let result = match rendered {
            Ok(Ok((image, scale))) => {
                let path = path.clone();
                cx.background_executor().spawn(async move { write_capture(image, scale, &path) }).await
            }
            Ok(Err(err)) | Err(err) => Err(err),
        };
        match result {
            Ok((w, h)) => println!("wrote {} ({w}\u{d7}{h})", path.display()),
            Err(err) => eprintln!("screenshot failed: {err:#}"),
        }
        // The billing probe may still be driving the `muse` TUI on a
        // background thread, and quitting now would orphan it: the child is
        // its own session leader, so it is reparented to pid 1 and the
        // `Pty` drop that would SIGKILL it never runs. Kill the live probe
        // first — the thread then finishes against a dead child and reaps it
        // on drop — and wait, bounded, for that drop, so the pid file is
        // gone too and no `muse` process outlives this quit.
        crate::tier::cleanup_probes();
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scheduling loops above all start from this: a step list that has
    /// not been armed yet reads exactly like one that already finished, so
    /// [`capture_and_quit`]'s first wait ("has it started") has to exist at
    /// all — this is the state it is waiting to see flip.
    #[test]
    fn a_fresh_capture_token_has_nothing_running_or_pending() {
        let token = CaptureToken::default();
        assert!(!token.pending_approval());
        assert!(!token.steps_running());
    }

    #[test]
    fn steps_running_round_trips() {
        let token = CaptureToken::default();
        token.set_steps_running(true);
        assert!(token.steps_running());
        token.set_steps_running(false);
        assert!(!token.steps_running());
    }

    #[test]
    fn pending_approval_round_trips() {
        let token = CaptureToken::default();
        token.set_pending_approval(true);
        assert!(token.pending_approval());
        token.set_pending_approval(false);
        assert!(!token.pending_approval());
    }

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
