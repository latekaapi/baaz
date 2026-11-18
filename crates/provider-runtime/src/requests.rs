//! Requests waiting for their answers, each with its own deadline.
//!
//! The pump completes a request by its key; the caller waits on its
//! [`Waiter`] until the answer lands, the deadline passes, or the child's
//! stream ends and [`RequestMap::close`] fails everything still open.
//!
//! "Closed" lives under the same lock as the waiters, so a request inserted
//! after the stream ended fails at once instead of sitting out its deadline
//! on a dead child: Muse used to check a flag, insert, and miss the reader
//! draining the map in between (docs/81 B PM-9).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};

/// Why a [`Waiter`] got no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// The deadline passed; the request is forgotten, and a late answer is
    /// [`RequestMap::complete`]'s `false`.
    Expired,
    /// The request was forgotten before any answer came.
    Forgotten,
}

/// One request in flight: where its answer lands, and until when.
pub struct Waiter<K, R> {
    key: K,
    rx: Receiver<R>,
    deadline: Instant,
}

impl<K, R> Waiter<K, R> {
    /// The request's key.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// The answer channel, for a caller that waits on its own terms (a
    /// detached request): the deadline is then the caller's, and an answer
    /// nobody reads is dropped when it lands.
    pub fn into_receiver(self) -> Receiver<R> {
        self.rx
    }
}

struct State<K, R> {
    waiters: HashMap<K, Sender<R>>,
    /// Set once by [`RequestMap::close`]: why every later insert fails.
    closed: Option<String>,
}

/// Requests in flight keyed by `K`, answered with `R`.
pub struct RequestMap<K, R> {
    state: Mutex<State<K, R>>,
}

impl<K: Eq + Hash + Clone, R> Default for RequestMap<K, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone, R> RequestMap<K, R> {
    /// An open, empty map.
    pub fn new() -> Self {
        Self { state: Mutex::new(State { waiters: HashMap::new(), closed: None }) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<K, R>> {
        // A panic elsewhere never wedges the requests: the map's state is
        // consistent after every statement.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Open a request under `key`, answered within `timeout`. `Err` with
    /// the close reason when the map is closed: no answer can come.
    pub fn insert(&self, key: K, timeout: Duration) -> Result<Waiter<K, R>, String> {
        let (tx, rx) = bounded(1);
        let mut state = self.lock();
        if let Some(reason) = &state.closed {
            return Err(reason.clone());
        }
        state.waiters.insert(key.clone(), tx);
        Ok(Waiter { key, rx, deadline: Instant::now() + timeout })
    }

    /// Deliver `answer` to the request under `key`. `false` when no such
    /// request is open (an unknown id, or one whose deadline passed).
    pub fn complete(&self, key: &K, answer: R) -> bool {
        self.deliver(key, answer).is_ok()
    }

    /// [`Self::complete`], handing the answer back when nobody waits for
    /// it, for the caller to report.
    pub fn deliver(&self, key: &K, answer: R) -> Result<(), R> {
        let waiter = self.lock().waiters.remove(key);
        match waiter {
            Some(waiter) => {
                let _ = waiter.send(answer);
                Ok(())
            }
            None => Err(answer),
        }
    }

    /// Drop the request under `key` unanswered (its write failed, or its
    /// caller gave up).
    pub fn forget(&self, key: &K) {
        self.lock().waiters.remove(key);
    }

    /// The stream ended: every open request gets `fail(reason)` now, and
    /// every later [`Self::insert`] fails with `reason`. Idempotent; the
    /// first reason stands.
    pub fn close(&self, reason: &str, fail: impl Fn(&str) -> R) {
        let waiters: Vec<Sender<R>> = {
            let mut state = self.lock();
            state.closed.get_or_insert_with(|| reason.to_owned());
            state.waiters.drain().map(|(_, waiter)| waiter).collect()
        };
        for waiter in waiters {
            let _ = waiter.send(fail(reason));
        }
    }

    /// Why the map closed, if it has.
    pub fn closed(&self) -> Option<String> {
        self.lock().closed.clone()
    }

    /// How many requests are open.
    pub fn len(&self) -> usize {
        self.lock().waiters.len()
    }

    /// Whether no request is open.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Wait for `waiter`'s answer until its deadline. An expired request is
    /// forgotten, so a late answer finds nobody.
    pub fn wait(&self, waiter: Waiter<K, R>) -> Result<R, Unanswered> {
        let timeout = waiter.deadline.saturating_duration_since(Instant::now());
        match waiter.rx.recv_timeout(timeout) {
            Ok(answer) => Ok(answer),
            Err(RecvTimeoutError::Timeout) => {
                self.forget(&waiter.key);
                // An answer that landed between the timeout and the forget
                // still counts.
                waiter.rx.try_recv().map_err(|_| Unanswered::Expired)
            }
            Err(RecvTimeoutError::Disconnected) => Err(Unanswered::Forgotten),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Map = RequestMap<u64, Result<&'static str, String>>;

    #[test]
    fn an_answer_reaches_its_waiter() {
        let map = Map::new();
        let waiter = map.insert(1, Duration::from_secs(5)).expect("open");
        assert!(map.complete(&1, Ok("done")));
        assert_eq!(map.wait(waiter), Ok(Ok("done")));
        assert!(!map.complete(&1, Ok("again")), "answered once");
    }

    /// THE HUNG REQUEST. A child that never answers: the deadline fires on
    /// time and the request is forgotten, so the late answer finds nobody.
    #[test]
    fn a_hung_request_expires_at_its_own_deadline() {
        let map = Map::new();
        let long = map.insert(1, Duration::from_secs(30)).expect("open");
        let short = map.insert(2, Duration::from_millis(100)).expect("open");
        let started = Instant::now();
        assert_eq!(map.wait(short), Err(Unanswered::Expired));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(100) && took < Duration::from_secs(2), "{took:?}");
        assert!(!map.complete(&2, Ok("late")), "the expired request is gone");
        assert_eq!(map.len(), 1, "the other request keeps its own deadline");
        assert!(map.complete(&1, Ok("still")));
        assert_eq!(map.wait(long), Ok(Ok("still")));
    }

    /// Closing fails the open requests at once, and a request inserted after
    /// the close fails at once rather than waiting out its deadline.
    #[test]
    fn closing_fails_open_requests_and_refuses_new_ones() {
        let map = Map::new();
        let open = map.insert(1, Duration::from_secs(30)).expect("open");
        map.close("the child exited", |reason| Err(reason.to_owned()));
        let started = Instant::now();
        assert_eq!(map.wait(open), Ok(Err("the child exited".to_owned())));
        assert_eq!(map.insert(2, Duration::from_secs(30)).err().as_deref(), Some("the child exited"));
        assert!(started.elapsed() < Duration::from_secs(1));
        map.close("again", |reason| Err(reason.to_owned()));
        assert_eq!(map.closed().as_deref(), Some("the child exited"), "the first reason stands");
    }

    /// The close races the insert from another thread many times: no insert
    /// ever ends up waiting on a closed map.
    #[test]
    fn no_insert_slips_past_a_close() {
        for round in 0..200u64 {
            let map = std::sync::Arc::new(Map::new());
            let closer = {
                let map = std::sync::Arc::clone(&map);
                std::thread::spawn(move || map.close("gone", |reason| Err(reason.to_owned())))
            };
            let outcome = match map.insert(round, Duration::from_secs(30)) {
                Err(_) => None,
                Ok(waiter) => {
                    closer.join().expect("closer");
                    Some(map.wait(waiter))
                }
            };
            if let Some(answer) = outcome {
                assert_eq!(answer, Ok(Err("gone".to_owned())), "round {round}");
            }
        }
    }
}
