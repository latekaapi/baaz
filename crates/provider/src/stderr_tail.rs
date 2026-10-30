//! A child's stderr, drained by its own thread into a short ring: the
//! agent's own words when it fails to start or dies ("No conversation
//! found…", a refused flag, a stack trace), which the "couldn't start" view
//! quotes (docs/71 §3). Draining also keeps a chatty child from blocking on
//! a full stderr pipe.
//!
//! One ring per adapter, shared by every child it spawns: a child that
//! dies before the adapter keeps it (a failed open) still leaves its words
//! here, and a respawn starts the ring afresh ([`StderrTail::reset`]).

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::STDERR_TAIL_LINES;

/// The last [`STDERR_TAIL_LINES`] non-blank stderr lines of the current
/// child. Cheap to clone: clones share the ring.
#[derive(Clone, Default)]
pub struct StderrTail {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    lines: Mutex<VecDeque<String>>,
    /// Bumped by [`StderrTail::reset`]: a drain thread of an earlier child
    /// stops writing once its generation is gone.
    generation: Mutex<u64>,
    closed: Mutex<bool>,
    ready: Condvar,
}

impl StderrTail {
    /// An empty ring.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start over for a new child: forget the old lines, retire the old
    /// drain thread. Returns the generation the new drain writes under.
    pub fn reset(&self) -> u64 {
        let mut generation = self.inner.generation.lock().expect("stderr generation");
        *generation += 1;
        self.inner.lines.lock().expect("stderr lines").clear();
        *self.inner.closed.lock().expect("stderr closed") = false;
        *generation
    }

    /// Reset, then drain `stderr` on a fresh thread named `name` until it
    /// closes. `echo` names the child in Baaz's own stderr when
    /// `BAAZ_TRACE=1` (`None` never echoes).
    pub fn drain(
        &self,
        stderr: impl Read + Send + 'static,
        name: &str,
        echo: Option<&'static str>,
    ) -> std::io::Result<()> {
        let generation = self.reset();
        let sink = self.clone();
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    if let Some(label) = echo.filter(|_| trace_enabled()) {
                        eprintln!("baaz: {label} stderr: {line}");
                    }
                    if !sink.push_for(generation, &line) {
                        return;
                    }
                }
                sink.close_for(generation);
            })
            .map(|_| ())
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    /// Keep one line (blank ones are dropped), for the current generation.
    pub fn push(&self, line: &str) {
        let generation = *self.inner.generation.lock().expect("stderr generation");
        self.push_for(generation, line);
    }

    fn push_for(&self, generation: u64, line: &str) -> bool {
        if *self.inner.generation.lock().expect("stderr generation") != generation {
            return false;
        }
        let line = line.trim_end();
        if line.trim().is_empty() {
            return true;
        }
        let mut lines = self.inner.lines.lock().expect("stderr lines");
        if lines.len() == STDERR_TAIL_LINES {
            lines.pop_front();
        }
        lines.push_back(line.to_owned());
        true
    }

    /// The stream closed: nothing more will land.
    pub fn close(&self) {
        let generation = *self.inner.generation.lock().expect("stderr generation");
        self.close_for(generation);
    }

    fn close_for(&self, generation: u64) {
        if *self.inner.generation.lock().expect("stderr generation") != generation {
            return;
        }
        *self.inner.closed.lock().expect("stderr closed") = true;
        self.inner.ready.notify_all();
    }

    /// The lines now, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.inner.lines.lock().expect("stderr lines").iter().cloned().collect()
    }

    /// The lines once stderr has closed or `wait` has passed (stdout and
    /// stderr close in either order), oldest first.
    pub fn settled(&self, wait: Duration) -> Vec<String> {
        let guard = self.inner.closed.lock().expect("stderr closed");
        drop(self.inner.ready.wait_timeout_while(guard, wait, |closed| !*closed).expect("stderr closed"));
        self.lines()
    }
}

/// Whether `BAAZ_TRACE=1` is set. Read once.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BAAZ_TRACE").is_ok_and(|v| v == "1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_last_lines_and_a_reset_forgets_them() {
        let tail = StderrTail::new();
        for n in 0..(STDERR_TAIL_LINES + 3) {
            tail.push(&format!("line {n}"));
        }
        tail.push("   ");
        let lines = tail.lines();
        assert_eq!(lines.len(), STDERR_TAIL_LINES);
        assert_eq!(lines.first().map(String::as_str), Some("line 3"));
        tail.reset();
        assert!(tail.lines().is_empty());
    }

    #[test]
    fn a_drained_stream_settles_once_it_closes() {
        let tail = StderrTail::new();
        tail.drain(std::io::Cursor::new(b"first\n\nsecond\n".to_vec()), "test-stderr", None).expect("drains");
        assert_eq!(tail.settled(Duration::from_secs(2)), vec!["first".to_owned(), "second".to_owned()]);
    }
}
