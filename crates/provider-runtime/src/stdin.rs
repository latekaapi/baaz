//! A child's stdin, owned by one writer thread.
//!
//! Nobody else touches the pipe: the pump and the dispatch side only
//! enqueue, so a large write blocked in `write_all` (the child not reading
//! its stdin until its stdout drains) can never stall the pump that drains
//! that stdout, and never holds a lock a dispatch also needs (docs/81 B
//! PM-8: Codex's pump used to write its replies under the stdin mutex that
//! dispatch held while blocked on a full pipe). Lines go out whole and in
//! enqueue order.
//!
//! Claude Code's writer, moved here so every adapter shares it.

use std::io::Write;
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender};

/// One line for the child's stdin: written whole, then `reply` answered
/// (when the sender waits) or, for a tagged line that failed, the owner
/// told which tag can never be answered.
struct WriteJob {
    line: String,
    reply: Option<Sender<std::io::Result<()>>>,
    tag: Option<String>,
}

/// The single writer of one child's stdin. Cheap to clone: clones share
/// the thread. The thread ends once every clone is dropped and its queue
/// has drained.
#[derive(Clone)]
pub struct StdinWriter {
    tx: Sender<WriteJob>,
}

/// The error every queued line gets once the writer thread is gone.
fn stopped() -> std::io::Error {
    std::io::Error::other("the child's stdin writer has stopped")
}

impl StdinWriter {
    /// Hand `stdin` to a fresh thread named `name`. Each line is written
    /// with a trailing newline. A failed write fails that line (its reply,
    /// or `on_failed(tag, error)` for a tagged line nobody waits on) and
    /// every line after it: a pipe that broke once stays broken.
    pub fn spawn(
        mut stdin: impl Write + Send + 'static,
        name: &str,
        on_failed: impl Fn(&str, &str) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = unbounded::<WriteJob>();
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let mut broken: Option<String> = None;
                for job in rx {
                    let written = match &broken {
                        Some(reason) => Err(std::io::Error::other(reason.clone())),
                        None => stdin
                            .write_all(job.line.as_bytes())
                            .and_then(|()| stdin.write_all(b"\n"))
                            .and_then(|()| stdin.flush()),
                    };
                    if let Err(error) = &written {
                        broken.get_or_insert_with(|| error.to_string());
                        if let Some(tag) = &job.tag {
                            on_failed(tag, &error.to_string());
                        }
                    }
                    if let Some(reply) = job.reply {
                        let _ = reply.send(written);
                    }
                }
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { tx })
    }

    /// Queue one line and return at once: the writer thread delivers it.
    /// `tag` names what the line carries (a request id), so a failed write
    /// reaches `on_failed` instead of leaving its request to a timeout.
    pub fn enqueue(&self, line: String, tag: Option<String>) -> std::io::Result<()> {
        self.tx.send(WriteJob { line, reply: None, tag }).map_err(|_| stopped())
    }

    /// Queue one line and hand back where its write result lands, without
    /// waiting for it: a caller holding a lock the pump needs waits only
    /// after releasing it.
    pub fn enqueue_with_reply(&self, line: String) -> Receiver<std::io::Result<()>> {
        let (reply, result) = bounded(1);
        if let Err(error) = self.tx.send(WriteJob { line, reply: Some(reply), tag: None }) {
            if let Some(reply) = error.into_inner().reply {
                let _ = reply.send(Err(stopped()));
            }
        }
        result
    }

    /// Queue one line and wait for its write (never from the pump, which
    /// never waits on a write).
    pub fn write_line(&self, line: &str) -> std::io::Result<()> {
        self.enqueue_with_reply(line.to_owned()).recv().unwrap_or_else(|_| Err(stopped()))
    }

    /// [`Self::write_line`], giving up after `timeout`: the line stays
    /// queued (and lands if the child ever reads), but the caller is free.
    /// A child that never reads its stdin cannot hold the caller longer.
    pub fn write_line_within(&self, line: &str, timeout: Duration) -> std::io::Result<()> {
        match self.enqueue_with_reply(line.to_owned()).recv_timeout(timeout) {
            Ok(written) => written,
            Err(RecvTimeoutError::Timeout) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the child is not reading its stdin",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(stopped()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A stdin that keeps whole lines and fails once told to.
    #[derive(Clone, Default)]
    struct Kept {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Write for Kept {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"));
            }
            self.bytes.lock().expect("bytes").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lines_land_whole_and_in_order() {
        let kept = Kept::default();
        let writer = StdinWriter::spawn(kept.clone(), "test-stdin", |_, _| {}).expect("writer");
        writer.enqueue("one".into(), None).expect("queued");
        writer.write_line("two").expect("written");
        assert_eq!(&*kept.bytes.lock().expect("bytes"), b"one\ntwo\n");
    }

    #[test]
    fn a_broken_pipe_fails_the_tagged_line_and_every_line_after() {
        let kept = Kept::default();
        kept.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let failed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&failed);
        let writer = StdinWriter::spawn(kept, "test-stdin", move |tag, _| sink.lock().expect("failed").push(tag.to_owned()))
            .expect("writer");
        writer.enqueue("a".into(), Some("req-1".into())).expect("queued");
        assert!(writer.write_line("b").is_err());
        writer.enqueue("c".into(), Some("req-2".into())).expect("queued");
        assert!(writer.write_line("d").is_err(), "still broken");
        assert_eq!(*failed.lock().expect("failed"), ["req-1", "req-2"]);
    }
}
