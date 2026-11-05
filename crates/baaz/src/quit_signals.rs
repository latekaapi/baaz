//! SIGTERM, SIGINT and SIGHUP quit Baaz the way the menu does (docs/74 §1).
//!
//! Once Chromium has started, its own handlers decide what a signal does, and
//! measured on CEF 154 inside Baaz: SIGTERM ended the process at once with
//! status 0, skipping `on_app_quit`, so pending state was not written and the
//! code-sign clone stayed behind (400 MB listed, every time). SIGINT and SIGHUP
//! did shut down, then died of a trace trap. So Baaz takes all three back after
//! Chromium starts, and one quit path serves them all.
//!
//! The handler only writes to a pipe (async-signal-safe). A second signal, or a
//! quit that has not finished in [`QUIT_BUDGET`], ends the process directly: a
//! hung UI thread must never make Baaz unkillable.
//!
//! Chromium installs its handlers some time after `CefInitialize` returns
//! (measured about 1.1 s after launch; a single re-take right after it lost
//! the race on an idle machine and won under load), so the watcher thread checks
//! the handlers for [`GUARD_FOR`] after launch and takes back any it finds
//! replaced. After that it only blocks on the pipe: nothing here wakes the app
//! at rest. A signal landing in the moment between Chromium's install and the
//! next check still ends the process Chromium's way, leaving a clone for the
//! next launch's sweep ([`crate::browser::clone_sweep`]).

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use gpui::App;

/// Long enough for `aui_cef::shutdown`'s own 3 s budget plus its settle.
const QUIT_BUDGET: Duration = Duration::from_secs(8);
/// How long after launch handlers are re-checked; the first page's lazy CEF
/// start lands well inside this (`warm_up_soon` runs a second after the window).
const GUARD_FOR: Duration = Duration::from_secs(60);
/// Chromium's install lands in the first seconds, so those are watched closely.
const GUARD_CLOSELY_FOR: Duration = Duration::from_secs(10);
const GUARD_CLOSELY_EVERY_MS: libc::c_int = 25;
const GUARD_EVERY_MS: libc::c_int = 250;
const SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNALED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(signal: libc::c_int) {
    if SIGNALED.swap(true, Ordering::SeqCst) {
        // SAFETY: `_exit` is async-signal-safe.
        unsafe { libc::_exit(128 + signal) };
    }
    let byte = signal as u8;
    // SAFETY: `write` is async-signal-safe; the byte lives across the call.
    unsafe { libc::write(WRITE_FD.load(Ordering::SeqCst), (&raw const byte).cast(), 1) };
}

/// Whether `signal`'s handler is still [`on_signal`].
fn handler_is_ours(signal: libc::c_int) -> bool {
    // SAFETY: a null new action only reads the current one.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut current) == 0
            && current.sa_sigaction == on_signal as *const () as usize
    }
}

/// Points `signal` at [`on_signal`].
fn take_handler(signal: libc::c_int) {
    // SAFETY: a zeroed `sigaction` is valid; the handler only does
    // async-signal-safe work.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut());
    }
}

/// Waits for the signal byte while keeping the handlers ours. Returns the
/// signal, or `None` if the pipe failed.
fn wait_for_signal(read_fd: libc::c_int) -> Option<u8> {
    let started = std::time::Instant::now();
    loop {
        let timeout = match started.elapsed() {
            elapsed if elapsed < GUARD_CLOSELY_FOR => GUARD_CLOSELY_EVERY_MS,
            elapsed if elapsed < GUARD_FOR => GUARD_EVERY_MS,
            _ => -1,
        };
        let mut waiter = libc::pollfd { fd: read_fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one live pollfd. Past the guard window this blocks for good.
        let ready = unsafe { libc::poll(&mut waiter, 1, timeout) };
        if ready > 0 {
            let mut byte = 0u8;
            // SAFETY: reads one byte into a live local.
            return (unsafe { libc::read(read_fd, (&raw mut byte).cast(), 1) } == 1).then_some(byte);
        }
        if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
        for signal in SIGNALS {
            if !handler_is_ours(signal) {
                crate::baaz_log!("quit signals: took signal {signal} back after {:?}", started.elapsed());
                take_handler(signal);
            }
        }
    }
}

/// Starts listening: the first signal asks the app to quit.
pub fn watch(cx: &mut App) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe` writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        crate::baaz_log!("quit signals: no pipe, signals keep their default behaviour");
        return;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    for fd in fds {
        // SAFETY: both descriptors were just opened. A child must not inherit
        // them: it would hold the pipe open past this process.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    WRITE_FD.store(write_fd, Ordering::SeqCst);
    let (asked_tx, asked_rx) = futures::channel::oneshot::channel::<()>();
    let spawned = std::thread::Builder::new().name("quit-signals".into()).spawn(move || {
        let Some(byte) = wait_for_signal(read_fd) else { return };
        let _ = asked_tx.send(());
        std::thread::sleep(QUIT_BUDGET);
        crate::baaz_log!("quit signals: the quit did not finish in {QUIT_BUDGET:?}, leaving");
        // SAFETY: ends the process; nothing after it runs.
        unsafe { libc::_exit(128 + libc::c_int::from(byte)) };
    });
    if let Err(error) = spawned {
        crate::baaz_log!("quit signals: no thread: {error}");
        return;
    }
    cx.spawn(async move |cx| {
        if asked_rx.await.is_ok() {
            crate::baaz_log!("quit signals: quitting");
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
    for signal in SIGNALS {
        take_handler(signal);
    }
}
