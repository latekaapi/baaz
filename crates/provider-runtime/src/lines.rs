//! A child's output, one line at a time, whatever the bytes are.
//!
//! `BufRead::lines()` ends the stream at the first invalid UTF-8 byte and
//! buffers a line of any length. On a pump that read as "the child exited"
//! while it was still running; on a stderr drain it dropped the read end and
//! the child met `EPIPE` (docs/81 B PM-6). [`LineReader`] reads with
//! `read_until` semantics instead: bytes become text lossily (U+FFFD for what
//! is not UTF-8), and a line longer than [`LINE_CAP`] is dropped — never
//! buffered past the cap — and reported as such, so the reader keeps going
//! in both cases and ends only at end of file or a read error.

use std::io::{BufRead as _, BufReader, ErrorKind, Read};

/// The longest line a [`LineReader`] keeps: 8 MB. A longer one is read to
/// its end and dropped, with [`LineFault::Oversize`] saying so.
pub const LINE_CAP: usize = 8 * 1024 * 1024;

/// What was wrong with a line the reader still delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineFault {
    /// Some bytes were not UTF-8; they read as U+FFFD.
    InvalidUtf8,
    /// The line ran past the cap and was dropped; `bytes` is its length.
    /// The line's text is empty.
    Oversize {
        /// How long the dropped line was, newline excluded.
        bytes: usize,
    },
}

/// One line of a child's output, newline (and a `\r` before it) removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line's text: lossy UTF-8; empty when the line was dropped.
    pub text: String,
    /// What was wrong with it, if anything.
    pub fault: Option<LineFault>,
}

impl Line {
    /// Whether the line was dropped for its length: its text is not the
    /// child's and must not be parsed.
    pub fn dropped(&self) -> bool {
        matches!(self.fault, Some(LineFault::Oversize { .. }))
    }

    /// A sentence for a log or a diagnostics ring when the line was
    /// dropped; `None` for a line that carries its text.
    pub fn note(&self) -> Option<String> {
        match self.fault {
            Some(LineFault::Oversize { bytes }) => {
                Some(format!("[dropped a {bytes}-byte line: longer than the {LINE_CAP}-byte cap]"))
            }
            _ => None,
        }
    }
}

/// Lines from `R`, until end of file or a read error. An iterator: `for
/// line in LineReader::new(stdout)`.
pub struct LineReader<R> {
    reader: BufReader<R>,
    cap: usize,
    buf: Vec<u8>,
    done: bool,
}

impl<R: Read> LineReader<R> {
    /// A reader with the standard [`LINE_CAP`].
    pub fn new(read: R) -> Self {
        Self::with_cap(read, LINE_CAP)
    }

    /// A reader dropping lines longer than `cap` bytes (tests use a small
    /// one).
    pub fn with_cap(read: R, cap: usize) -> Self {
        Self { reader: BufReader::new(read), cap, buf: Vec::new(), done: false }
    }

    /// The next line, or `None` at end of file or on a read error.
    fn next_line(&mut self) -> Option<Line> {
        if self.done {
            return None;
        }
        self.buf.clear();
        let mut length = 0usize;
        let mut over = false;
        let mut ended = false;
        loop {
            let available = match self.reader.fill_buf() {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.done = true;
                    ended = true;
                    break;
                }
            };
            if available.is_empty() {
                self.done = true;
                ended = true;
                break;
            }
            let (chunk, found) = match available.iter().position(|byte| *byte == b'\n') {
                Some(at) => (&available[..at], true),
                None => (available, false),
            };
            let taken = chunk.len();
            if !over {
                if self.buf.len() + taken <= self.cap {
                    self.buf.extend_from_slice(chunk);
                } else {
                    over = true;
                    self.buf = Vec::new();
                }
            }
            length += taken;
            self.reader.consume(taken + usize::from(found));
            if found {
                break;
            }
        }
        // End of file with nothing read: no last line.
        if ended && length == 0 {
            return None;
        }
        if over {
            return Some(Line { text: String::new(), fault: Some(LineFault::Oversize { bytes: length }) });
        }
        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        Some(match String::from_utf8(std::mem::take(&mut self.buf)) {
            Ok(text) => Line { text, fault: None },
            Err(error) => Line {
                text: String::from_utf8_lossy(error.as_bytes()).into_owned(),
                fault: Some(LineFault::InvalidUtf8),
            },
        })
    }
}

impl<R: Read> Iterator for LineReader<R> {
    type Item = Line;

    fn next(&mut self) -> Option<Line> {
        self.next_line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(bytes: &[u8], cap: usize) -> Vec<Line> {
        LineReader::with_cap(std::io::Cursor::new(bytes.to_vec()), cap).collect()
    }

    #[test]
    fn plain_lines_and_a_last_line_without_a_newline() {
        let lines = read(b"one\r\ntwo\n\nthree", 64);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "", "three"]);
        assert!(lines.iter().all(|line| line.fault.is_none()));
    }

    #[test]
    fn invalid_utf8_reads_lossily_and_the_stream_goes_on() {
        let lines = read(b"ok\n\xff\xfe{\"a\":1}\nafter\n", 64);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].text, "\u{fffd}\u{fffd}{\"a\":1}");
        assert_eq!(lines[1].fault, Some(LineFault::InvalidUtf8));
        assert_eq!(lines[2].text, "after");
    }

    #[test]
    fn an_oversize_line_is_dropped_with_a_note_and_the_next_one_reads() {
        let mut bytes = b"short\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 100));
        bytes.extend_from_slice(b"\nnext\n");
        let lines = read(&bytes, 10);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1], Line { text: String::new(), fault: Some(LineFault::Oversize { bytes: 100 }) });
        assert!(lines[1].dropped());
        assert!(lines[1].note().expect("a note").contains("100-byte"));
        assert_eq!(lines[2].text, "next");
    }

    #[test]
    fn a_line_exactly_at_the_cap_is_kept() {
        let mut bytes = vec![b'y'; 10];
        bytes.push(b'\n');
        assert_eq!(read(&bytes, 10)[0].text, "y".repeat(10));
    }

    /// A reader that fails after its bytes: the lines before the error
    /// arrive, then the iterator ends.
    #[test]
    fn a_read_error_ends_the_stream() {
        struct Failing(std::io::Cursor<Vec<u8>>);
        impl Read for Failing {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.read(buf)? {
                    0 => Err(std::io::Error::other("gone")),
                    n => Ok(n),
                }
            }
        }
        let lines: Vec<Line> = LineReader::new(Failing(std::io::Cursor::new(b"a\nb\n".to_vec()))).collect();
        assert_eq!(lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }
}
