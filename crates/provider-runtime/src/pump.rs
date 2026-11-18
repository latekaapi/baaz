//! The pump: a child's stdout read on its own thread, one guarded line at a
//! time.
//!
//! Every adapter reads its child's output the same way: lines through
//! [`LineReader`] (so garbage never ends the stream early), each line's
//! step through [`guard_line`] (so a panic in a fold ends the session as a
//! lost connection instead of killing the thread silently, docs/81 B PM-3),
//! and exactly one call to `end` when the stream is over.

use std::io::Read;

use crate::guard::guard_line;
use crate::lines::{Line, LineReader};

/// What a step wants next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Read the next line.
    Continue,
    /// Stop reading: the stream is treated as ended.
    Stop,
}

/// Why a pump stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PumpEnd {
    /// The stream ended (end of file, a read error, or a step's
    /// [`Flow::Stop`]).
    Closed,
    /// A step panicked; the message is the panic's. The caller clears the
    /// poison its locks may carry and reports the connection lost.
    Panicked(String),
}

/// Read `reader` to its end on this thread: each line through `step`
/// under [`guard_line`], then `end` once.
pub fn run_pump(reader: impl Read, mut step: impl FnMut(Line) -> Flow, end: impl FnOnce(PumpEnd)) {
    for line in LineReader::new(reader) {
        let mut flow = Flow::Continue;
        if let Err(message) = guard_line(|| flow = step(line)) {
            end(PumpEnd::Panicked(message));
            return;
        }
        if flow == Flow::Stop {
            break;
        }
    }
    end(PumpEnd::Closed);
}

/// [`run_pump`] on a fresh thread named `name`. The handle is the caller's
/// to keep or drop: a pump ends with its stream, so nothing joins it on a
/// hang-up (a grandchild holding the pipe would make that join hang).
pub fn spawn_pump<R: Read + Send + 'static>(
    name: &str,
    reader: R,
    step: impl FnMut(Line) -> Flow + Send + 'static,
    end: impl FnOnce(PumpEnd) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || run_pump(reader, step, end))
        .map_err(|error| std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn pump(bytes: &[u8], panic_on: Option<&'static str>) -> (Vec<String>, PumpEnd) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ended = Arc::new(Mutex::new(None));
        let (sink, last) = (Arc::clone(&seen), Arc::clone(&ended));
        spawn_pump(
            "test-pump",
            std::io::Cursor::new(bytes.to_vec()),
            move |line| {
                if Some(line.text.as_str()) == panic_on {
                    panic!("bad frame");
                }
                sink.lock().expect("seen").push(line.text);
                if seen_stop(&sink) { Flow::Stop } else { Flow::Continue }
            },
            move |end| *last.lock().expect("end") = Some(end),
        )
        .expect("pump")
        .join()
        .expect("joins");
        let lines = seen.lock().expect("seen").clone();
        let end = ended.lock().expect("end").clone().expect("end called once");
        (lines, end)
    }

    fn seen_stop(seen: &Arc<Mutex<Vec<String>>>) -> bool {
        seen.lock().expect("seen").last().is_some_and(|line| line == "stop")
    }

    #[test]
    fn every_line_then_one_close() {
        let (lines, end) = pump(b"a\n\xffb\nc\n", None);
        assert_eq!(lines, ["a", "\u{fffd}b", "c"]);
        assert_eq!(end, PumpEnd::Closed);
    }

    #[test]
    fn a_panicking_step_ends_the_pump_with_its_message() {
        let (lines, end) = pump(b"a\nboom\nnever\n", Some("boom"));
        assert_eq!(lines, ["a"]);
        assert_eq!(end, PumpEnd::Panicked("bad frame".into()));
    }

    #[test]
    fn a_step_can_stop_the_stream() {
        let (lines, end) = pump(b"a\nstop\nnever\n", None);
        assert_eq!(lines, ["a", "stop"]);
        assert_eq!(end, PumpEnd::Closed);
    }
}
