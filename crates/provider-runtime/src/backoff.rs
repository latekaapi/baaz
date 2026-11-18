//! Respawn backoff: how long to wait before starting a crashed child
//! again, and when to stop trying.
//!
//! Muse's one shared child used to be respawned the moment it exited, with
//! no limit: a child that died on start crash-looped as fast as it could
//! spawn (docs/81 B PM-7). [`RespawnBackoff`] counts the crashes inside a
//! sliding window: the first respawns at once, each next one waits twice as
//! long (up to a cap), and once `limit` crashes fall inside the window it
//! says [`Respawn::GiveUp`] — the wire is reported down and a person's
//! Reconnect ([`RespawnBackoff::forgive`]) starts the count afresh.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// What to do about a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Respawn {
    /// Respawn after this long.
    After(Duration),
    /// Stop: `crashes` crashes inside `window`.
    GiveUp {
        /// How many crashes the window holds.
        crashes: usize,
        /// The window they fell in.
        window: Duration,
    },
}

/// The crash count and its policy.
#[derive(Clone, Debug)]
pub struct RespawnBackoff {
    first: Duration,
    cap: Duration,
    window: Duration,
    limit: usize,
    crashes: VecDeque<Instant>,
}

impl Default for RespawnBackoff {
    /// At once, then 0.5 s, 1 s, 2 s…, capped at 8 s; five crashes in two
    /// minutes and the wire is down.
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(8), Duration::from_secs(120), 5)
    }
}

impl RespawnBackoff {
    /// The second respawn waits `first`, each later one twice the one
    /// before, never more than `cap`; `limit` crashes inside `window` give
    /// up.
    pub fn new(first: Duration, cap: Duration, window: Duration, limit: usize) -> Self {
        Self { first, cap, window, limit: limit.max(1), crashes: VecDeque::new() }
    }

    /// A crash at `now`: respawn after how long, or give up.
    pub fn on_crash(&mut self, now: Instant) -> Respawn {
        while self.crashes.front().is_some_and(|at| now.saturating_duration_since(*at) > self.window) {
            self.crashes.pop_front();
        }
        self.crashes.push_back(now);
        let count = self.crashes.len();
        if count >= self.limit {
            return Respawn::GiveUp { crashes: count, window: self.window };
        }
        if count == 1 {
            return Respawn::After(Duration::ZERO);
        }
        let doublings = u32::try_from(count - 2).unwrap_or(u32::MAX).min(16);
        Respawn::After(self.first.saturating_mul(1 << doublings).min(self.cap))
    }

    /// Start the count afresh (a person asked to reconnect).
    pub fn forgive(&mut self) {
        self.crashes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_grow_then_the_wire_goes_down() {
        let mut backoff = RespawnBackoff::default();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        assert_eq!(backoff.on_crash(at(0)), Respawn::After(Duration::ZERO));
        assert_eq!(backoff.on_crash(at(1)), Respawn::After(Duration::from_millis(500)));
        assert_eq!(backoff.on_crash(at(2)), Respawn::After(Duration::from_secs(1)));
        assert_eq!(backoff.on_crash(at(3)), Respawn::After(Duration::from_secs(2)));
        assert_eq!(
            backoff.on_crash(at(4)),
            Respawn::GiveUp { crashes: 5, window: Duration::from_secs(120) }
        );
        backoff.forgive();
        assert_eq!(backoff.on_crash(at(5)), Respawn::After(Duration::ZERO), "a Reconnect starts afresh");
    }

    #[test]
    fn crashes_outside_the_window_are_forgotten() {
        let mut backoff = RespawnBackoff::new(Duration::from_secs(1), Duration::from_secs(3), Duration::from_secs(10), 3);
        let start = Instant::now();
        backoff.on_crash(start);
        backoff.on_crash(start + Duration::from_secs(1));
        // Twenty seconds later both have aged out: this is a first crash.
        assert_eq!(backoff.on_crash(start + Duration::from_secs(21)), Respawn::After(Duration::ZERO));
    }

    #[test]
    fn the_wait_is_capped() {
        let mut backoff = RespawnBackoff::new(Duration::from_secs(1), Duration::from_secs(3), Duration::from_secs(600), 100);
        let start = Instant::now();
        let mut last = Respawn::After(Duration::ZERO);
        for n in 0..10 {
            last = backoff.on_crash(start + Duration::from_secs(n));
        }
        assert_eq!(last, Respawn::After(Duration::from_secs(3)));
    }
}
