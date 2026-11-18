//! Fault injection on the process layer (docs/82 5.1): a killed child,
//! garbage on stdout, a child that never reads its stdin, a request that is
//! never answered, and a stubborn process group — each against a real
//! stand-in child (`tests/support/stand-in.sh`), never a real CLI.

use std::io::{BufRead as _, BufReader};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use provider_runtime::pump::{spawn_pump, Flow, PumpEnd};
use provider_runtime::{
    Exit, Identity, LineFault, RequestMap, Spawned, StdinWriter, Unanswered, TERM_GRACE,
};

mod support;
use support::{gone_within, stand_in, Reap};

type Answer = Result<String, String>;

/// THE KILLED CHILD. SIGKILL under a request in flight: the pump ends,
/// the request fails at once with the exit — not at its 30 s deadline —
/// a later request fails at once too, and the hang-up returns promptly.
#[test]
fn a_killed_child_fails_its_requests_at_once_and_holds_nothing() {
    let Spawned { process, stdin, stdout, .. } = stand_in("echo");
    let _reap = Reap::process(&process);
    let requests: Arc<RequestMap<u64, Answer>> = Arc::new(RequestMap::new());
    let ended = Arc::new(Mutex::new(None));
    let (pump_requests, pump_ended) = (Arc::clone(&requests), Arc::clone(&ended));
    let end_requests = Arc::clone(&requests);
    spawn_pump(
        "test-pump",
        stdout,
        move |line| {
            if let Some((id, rest)) = line.text.split_once(' ') {
                if let Ok(id) = id.parse() {
                    pump_requests.complete(&id, Ok(rest.to_owned()));
                }
            }
            Flow::Continue
        },
        move |end| {
            end_requests.close("the child exited", |reason| Err(reason.to_owned()));
            *pump_ended.lock().expect("end") = Some(end);
        },
    )
    .expect("pump");
    let writer = StdinWriter::spawn(stdin, "test-stdin", |_, _| {}).expect("writer");

    // One round trip works.
    let waiter = requests.insert(1, Duration::from_secs(30)).expect("open");
    writer.enqueue("1 pong".into(), None).expect("queued");
    assert_eq!(requests.wait(waiter), Ok(Ok("pong".into())));

    // A request the child will never answer, then the kill.
    let waiter = requests.insert(2, Duration::from_secs(30)).expect("open");
    // SAFETY: SIGKILL to our own stand-in child.
    unsafe { libc::kill(process.identity().pid, libc::SIGKILL) };
    let started = Instant::now();
    assert_eq!(requests.wait(waiter), Ok(Err("the child exited".into())));
    assert!(started.elapsed() < Duration::from_secs(5), "failed at the exit, not the deadline");
    assert_eq!(*ended.lock().expect("end"), Some(PumpEnd::Closed));
    assert_eq!(requests.insert(3, Duration::from_secs(30)).err().as_deref(), Some("the child exited"));
    assert_eq!(process.wait_exit(Duration::from_secs(5)), Some(Exit::Signal(libc::SIGKILL)));
    let started = Instant::now();
    process.terminate(TERM_GRACE);
    assert!(started.elapsed() < Duration::from_secs(1), "nothing to wait for");
}

/// THE GARBAGE LINE. Invalid UTF-8 reads lossily and a 9 MB line is
/// dropped with a note; the line after both still arrives.
#[test]
fn garbage_on_stdout_neither_ends_nor_floods_the_pump() {
    let Spawned { process, stdout, .. } = stand_in("garbage");
    let _reap = Reap::process(&process);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    spawn_pump(
        "test-pump",
        stdout,
        move |line| {
            sink.lock().expect("lines").push(line);
            Flow::Continue
        },
        move |end| {
            let _ = done_tx.send(end);
        },
    )
    .expect("pump");
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(10)), Ok(PumpEnd::Closed));
    let lines = lines.lock().expect("lines");
    assert_eq!(lines.len(), 4, "{:?}", lines.iter().map(|line| (&line.text[..line.text.len().min(20)], line.fault)).collect::<Vec<_>>());
    assert_eq!(lines[0].text, "first");
    assert_eq!(lines[1].text, "\u{fffd}\u{fffd}{\"bad\":1}");
    assert_eq!(lines[1].fault, Some(LineFault::InvalidUtf8));
    assert_eq!(lines[2].fault, Some(LineFault::Oversize { bytes: 9_000_000 }));
    assert!(lines[2].text.is_empty() && lines[2].note().is_some());
    assert_eq!(lines[3].text, "last");
}

/// THE FULL PIPE. A child that never reads its stdin: queueing megabytes
/// returns at once, a caller waiting on a write is freed at its bound, a
/// lock the dispatch side takes is never held by the writer, and the
/// hang-up breaks the pipe so every queued line fails rather than hangs.
#[test]
fn a_child_that_never_reads_stdin_stalls_nobody() {
    let Spawned { process, stdin, .. } = stand_in("deaf");
    let _reap = Reap::process(&process);
    let failed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&failed);
    let writer = StdinWriter::spawn(stdin, "test-stdin", move |tag, _| sink.lock().expect("failed").push(tag.to_owned()))
        .expect("writer");
    // What dispatch would hold while it sends: the writer never takes it.
    let dispatch = Mutex::new(());
    let started = Instant::now();
    {
        let _held = dispatch.lock().expect("dispatch");
        for n in 0..8 {
            writer.enqueue("x".repeat(512 * 1024), Some(format!("req-{n}"))).expect("queued");
        }
    }
    assert!(started.elapsed() < Duration::from_millis(500), "queueing never waits on the pipe");
    let started = Instant::now();
    let error = writer.write_line_within("one more", Duration::from_millis(200)).expect_err("never written");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(dispatch.try_lock().is_ok(), "the dispatch lock is free throughout");
    // The hang-up breaks the pipe: the blocked write fails, and so does
    // every tagged line behind it.
    process.terminate(TERM_GRACE);
    let deadline = Instant::now() + Duration::from_secs(5);
    while failed.lock().expect("failed").len() < 7 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(failed.lock().expect("failed").len() >= 7, "{:?}", failed.lock().expect("failed"));
}

/// THE HUNG REQUEST. A live child that never answers: the request's own
/// deadline fires; once the stream is closed a new request fails at once.
#[test]
fn a_request_a_live_child_never_answers_expires_on_time() {
    let Spawned { process, stdin, .. } = stand_in("deaf");
    let _reap = Reap::process(&process);
    let writer = StdinWriter::spawn(stdin, "test-stdin", |_, _| {}).expect("writer");
    let requests: RequestMap<u64, Answer> = RequestMap::new();
    let waiter = requests.insert(1, Duration::from_millis(150)).expect("open");
    writer.enqueue("1 ping".into(), Some("1".into())).expect("queued");
    let started = Instant::now();
    assert_eq!(requests.wait(waiter), Err(Unanswered::Expired));
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(140) && took < Duration::from_secs(2), "{took:?}");
    requests.close("the child exited", |reason| Err(reason.to_owned()));
    let started = Instant::now();
    assert!(requests.insert(2, Duration::from_secs(30)).is_err());
    assert!(started.elapsed() < Duration::from_millis(50));
}

/// THE GROUP KILL. A child that ignores SIGTERM and left a grandchild that
/// ignores it too: after the hang-up both are gone, the leader reaped.
#[test]
fn a_hang_up_ends_a_stubborn_child_and_its_grandchild() {
    let Spawned { process, stdout, .. } = stand_in("stubborn");
    let mut first = String::new();
    BufReader::new(stdout).read_line(&mut first).expect("the grandchild's pid");
    let grand = Identity::of(first.trim().parse().expect("a pid")).expect("the grandchild runs");
    let _reap = Reap::process(&process).and(grand);
    assert!(grand.is_alive() && process.identity().is_alive());
    let started = Instant::now();
    process.terminate(Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
    assert_eq!(process.try_exit(), Some(Exit::Signal(libc::SIGKILL)), "SIGTERM was ignored; SIGKILL ended it");
    assert!(gone_within(grand, Duration::from_secs(5)), "the grandchild went with its group");
}
