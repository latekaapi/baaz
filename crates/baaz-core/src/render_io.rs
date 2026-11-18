//! The blocking-read counter behind the right pane's purity tests: every
//! `git` read ([`crate::git_ops`]) and every file-walk `read_dir` in the
//! app counts one here in test builds, so a test can prove a render or a
//! refresh skipped the filesystem. Production has no counter: [`note_io`]
//! compiles to nothing.

#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Blocking reads (`git` spawns, `read_dir` calls) since process start or
/// the last [`reset_io_count`].
#[cfg(any(test, feature = "test-support"))]
static RENDER_IO_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Count one blocking read toward [`io_count`] in test builds; nothing in
/// production, where the counter does not exist.
pub fn note_io() {
    #[cfg(any(test, feature = "test-support"))]
    RENDER_IO_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// How many blocking reads have run since process start (or the last
/// [`reset_io_count`]).
#[cfg(any(test, feature = "test-support"))]
pub fn io_count() -> usize {
    RENDER_IO_COUNT.load(Ordering::Relaxed)
}

/// Zero [`io_count`].
#[cfg(any(test, feature = "test-support"))]
pub fn reset_io_count() {
    RENDER_IO_COUNT.store(0, Ordering::Relaxed);
}
