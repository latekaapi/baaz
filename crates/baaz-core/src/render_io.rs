//! The blocking-read counter behind the right pane's purity tests: every
//! `git` read ([`crate::git_ops`]) and every file-walk `read_dir` in the
//! app counts one here in test builds, so a test can prove a render or a
//! refresh skipped the filesystem. Production has no counter: [`note_io`]
//! compiles to nothing.

// Per thread: the tests run in parallel, and a process-wide count picked
// up other tests' reads (right::tests::unchanged_expanded_directories_are_not_reread
// saw 4 for 3 in Stage 8's gate). gpui's test dispatcher runs background
// work on the test's own thread, so a background read still counts (the
// app's purity tests assert one does).
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Blocking reads (`git` spawns, `read_dir` calls) on this thread since
    /// it started or the last [`reset_io_count`].
    static RENDER_IO_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count one blocking read toward [`io_count`] in test builds; nothing in
/// production, where the counter does not exist.
pub fn note_io() {
    #[cfg(any(test, feature = "test-support"))]
    RENDER_IO_COUNT.with(|count| count.set(count.get() + 1));
}

/// How many blocking reads have run on this thread since it started (or
/// the last [`reset_io_count`]).
#[cfg(any(test, feature = "test-support"))]
pub fn io_count() -> usize {
    RENDER_IO_COUNT.with(std::cell::Cell::get)
}

/// Zero [`io_count`] for this thread.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_io_count() {
    RENDER_IO_COUNT.with(|count| count.set(0));
}
