//! T1 (2026-10-06): ⌘W / the red dot froze the app for seconds.
//!
//! baaz is a binary, so this suite cannot drive its internals (the live
//! pty-drop and close/quit timing bounds are pinned by the `t1_close_tests`
//! unit tests beside the code in `tier.rs`, which spawn real children).
//! What this suite pins is the shipped wiring, by reading the source: the
//! close path must signal live probes without ever waiting, and `Pty::drop`
//! must close the master fd before reaping with a bounded non-blocking
//! `waitpid`, never a blocking `Child::wait`.

use std::fs;
use std::path::{Path, PathBuf};

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(name: &str) -> String {
    fs::read_to_string(src().join(name)).expect("baaz source is readable")
}

/// The byte range of `needle`'s enclosing brace block: from `needle` to the
/// first following line that is exactly `}`.
fn block_from(text: &str, needle: &str) -> String {
    let start = text.find(needle).unwrap_or_else(|| panic!("{needle} is present"));
    let rest = &text[start..];
    let end = rest.find("\n}\n").expect("the block ends");
    rest[..end].to_owned()
}

/// `Pty::drop` closes the master fd FIRST (so an exiting child's final tty
/// close can complete) and reaps with bounded `waitpid(WNOHANG)` — never a
/// blocking `Child::wait`, which deadlocked parent against child.
#[test]
fn pty_drop_closes_master_first_and_never_blocks() {
    let tier = read("tier.rs");
    let drop = block_from(&tier, "impl Drop for Pty");
    assert!(drop.contains("libc::close(self.master)"), "the drop closes the master fd");
    assert!(drop.contains("WNOHANG"), "the drop reaps with waitpid(WNOHANG)");
    assert!(!drop.contains("self.child.wait()"), "no blocking Child::wait in the drop");
    let close = drop.find("libc::close(self.master)").unwrap();
    let reap = drop.find("waitpid").unwrap();
    assert!(close < reap, "master closes before the reap starts");
}

/// The window-close cleanup signals only: no wait, no sleep anywhere in it.
#[test]
fn window_close_cleanup_never_waits() {
    let tier = read("tier.rs");
    assert!(tier.contains("pub fn cleanup_probes_for_close"), "the close-path cleanup exists");
    let close = block_from(&tier, "pub fn cleanup_probes_for_close");
    assert!(close.contains("kill_live_probes()"), "closing still signals live probes");
    assert!(!close.contains("wait_for_probes_gone"), "closing never waits for probes");
    assert!(!close.contains("sleep"), "nothing in the close path may sleep");
    assert!(
        tier.contains("Nothing in the close path may sleep"),
        "the no-sleep rule is documented at the hook"
    );
}

/// The quit cleanup keeps a bounded wait, shortened past the old 3 s, and
/// skips it entirely when no probe is live.
#[test]
fn quit_cleanup_keeps_a_short_bounded_wait() {
    let tier = read("tier.rs");
    let quit = block_from(&tier, "pub fn cleanup_probes()");
    assert!(quit.contains("wait_for_probes_gone"), "quitting still waits boundedly");
    assert!(!tier.contains("from_secs(3)"), "the old 3 s close wait is gone");
}

/// Both window-close hooks (bench and shell) signal without waiting; both
/// app-quit hooks keep the bounded quit cleanup.
#[test]
fn main_close_hooks_signal_and_quit_hooks_wait() {
    let main = read("main.rs");
    assert_eq!(
        main.matches("cleanup_probes_for_close()").count(),
        2,
        "both on_window_should_close hooks signal without waiting"
    );
    assert_eq!(
        main.matches("crate::tier::cleanup_probes();").count(),
        2,
        "both on_app_quit hooks keep the bounded quit cleanup"
    );
}

/// The ⌘W action routes to the same no-wait close path as the red dot; only
/// the real Quit action keeps the bounded wait.
#[test]
fn close_window_action_routes_to_the_no_wait_path() {
    let app = read("app.rs");
    assert!(app.contains("cleanup_probes_for_close()"), "⌘W signals without waiting");
    assert_eq!(
        app.matches("crate::tier::cleanup_probes();").count(),
        1,
        "only QuitApp keeps the bounded quit cleanup"
    );
    // The ⌘W handler ends at its hide; slicing to the enclosing function's
    // end would bleed into the QuitApp handler below, which legitimately
    // keeps the waiting cleanup.
    let start = app.find("|_: &CloseWindow").expect("the ⌘W handler is present");
    let end = app[start..].find("cx.defer(|cx| cx.hide());").expect("the ⌘W handler hides");
    let close = &app[start..start + end];
    assert!(close.contains("cleanup_probes_for_close()"), "⌘W runs the close-path signal");
    assert!(!close.contains("cleanup_probes();"), "⌘W never runs the waiting cleanup");
}
