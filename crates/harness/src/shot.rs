//! `--screenshot`: render the window off-screen once it has settled,
//! downsample to logical pixels and quit.
//!
//! Lifted from the gallery's own capture path (`aui-gallery/src/shot.rs`) so a
//! harness PNG and a gallery PNG are the same kind of image: 1×, no window
//! chrome, no pointer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::{path::PathBuf, time::Duration};

use gpui::{App, WindowHandle};
use gpui_kit::component::Root;

/// Whether the open session is showing a pending approval right now.
///
/// A flag rather than a callback because the capture runs outside the entity
/// tree: [`capture_and_quit`] holds a `WindowHandle`, not the session. The
/// session sets it on every frame it renders.
static PENDING_APPROVAL: AtomicBool = AtomicBool::new(false);

/// Whether a `--steps` list is still running.
///
/// A scripted capture used to race its own script: the delay is measured from
/// the first frame, and a step list containing a `wait:` outlives it, so the
/// PNG showed the window before the steps that were the point of taking it.
static STEPS_RUNNING: AtomicBool = AtomicBool::new(false);

/// How long a capture that is waiting for an approval will wait.
const APPROVAL_CEILING: Duration = Duration::from_secs(15);
/// How often it looks.
const POLL: Duration = Duration::from_millis(100);

/// Called by the session view each frame: is a card waiting on the person?
pub fn set_pending_approval(pending: bool) {
    PENDING_APPROVAL.store(pending, Ordering::Relaxed);
}

/// Called by the application around its `--steps` and `--login-steps` loops.
pub fn set_steps_running(running: bool) {
    STEPS_RUNNING.store(running, Ordering::Relaxed);
}

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

/// Waits for the first frames, captures the window and exits the process.
///
/// `await_steps` is set whenever `--steps` or `--login-steps` were given, and
/// `await_approval` when one of them is a `shell:`, which raises a real,
/// server-minted approval over a live wire. The fixed delay is the right wait
/// for a fold that is already in memory and the wrong one for a script or a
/// round-trip to a child process: `docs/images/phase4-approval-stage1-*.png`
/// were captured before the card arrived and showed the shell card alone
/// (finding F9). So the capture waits for the condition, and then for the
/// same settling delay it would have used anyway.
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
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        settle(cx, delay).await;
        let mut waited = false;
        if await_steps {
            let deadline = std::time::Instant::now() + APPROVAL_CEILING;
            // The flag is only raised once the session is open, so the wait is
            // for "the steps have run", not "the steps are not running yet".
            while !STEPS_RUNNING.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                cx.background_executor().timer(POLL).await;
            }
            while STEPS_RUNNING.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                cx.background_executor().timer(POLL).await;
            }
            waited = true;
        }
        if await_approval {
            let deadline = std::time::Instant::now() + APPROVAL_CEILING;
            while !PENDING_APPROVAL.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                cx.background_executor().timer(POLL).await;
            }
            if !PENDING_APPROVAL.load(Ordering::Relaxed) {
                eprintln!("harness: no approval arrived in {APPROVAL_CEILING:?}; capturing anyway");
            }
            waited = true;
        }
        if waited {
            // Whatever arrived animates in; give it the same settling time the
            // first frames got — polled, for the same starvation reason.
            settle(cx, delay).await;
        }
        let result = cx.update(|cx| {
            handle.update(cx, |_root, window, _cx| {
                let scale = window.scale_factor();
                let image = window.render_to_image()?;
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
                image.save(&path)?;
                anyhow::Ok((target_w, target_h))
            })
        });
        match result {
            Ok(Ok((w, h))) => println!("wrote {} ({w}\u{d7}{h})", path.display()),
            Ok(Err(err)) => eprintln!("screenshot failed: {err:#}"),
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
