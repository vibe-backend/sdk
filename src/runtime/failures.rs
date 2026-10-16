//! Failure counting for `challenge: { when: after-failures }` routes
//! (decision 133, §4A.3, #201).
//!
//! A failure is a `401` answered by the route itself (for login routes,
//! `INVALID_CREDENTIALS`). Failures are counted per tenant, route and client
//! IP bucket (IPv6 /64) in a sliding window of 15 minutes. Once a key has
//! reached the route's `after-failures` threshold inside the window, every
//! further request needs a verified challenge until enough failures leave the
//! window. A successful request does not reset the count, so interleaving
//! requests that succeed does not avoid the challenge.
//!
//! This store is per process. It fails closed: when it is full of live keys,
//! a key it cannot track is treated as having reached the threshold.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Sliding window for failures (§4A.3: 3 failures in 15 minutes).
pub const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Default threshold when the lock gives no `after-failures: <n>`.
pub const DEFAULT_AFTER_FAILURES: u32 = 3;
/// Most keys held at a time.
pub const FAILURE_KEYS: usize = 100_000;

pub struct FailureCounter {
    window: Duration,
    capacity: usize,
    inner: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Default for FailureCounter {
    fn default() -> Self {
        Self::new(FAILURE_WINDOW, FAILURE_KEYS)
    }
}

/// The counting key of a request.
pub fn failure_key(tenant: &str, route: &str, ip: &str) -> String {
    format!("{tenant}\u{1f}{route}\u{1f}{ip}")
}

impl FailureCounter {
    pub fn new(window: Duration, capacity: usize) -> Self {
        Self {
            window,
            capacity: capacity.max(1),
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn prune(&self, list: &mut VecDeque<Instant>, now: Instant) {
        while list
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            list.pop_front();
        }
    }

    /// Whether the next request for `key` needs a challenge. Fails closed: a
    /// poisoned lock, or a full store that has no slot for `key`, answers
    /// `true`.
    pub fn challenge_required(&self, key: &str, threshold: u32, now: Instant) -> bool {
        if threshold == 0 {
            return true;
        }
        let Ok(mut map) = self.inner.lock() else {
            return true;
        };
        match map.get_mut(key) {
            Some(list) => {
                self.prune(list, now);
                list.len() >= threshold as usize
            }
            None => map.len() >= self.capacity && !self.make_room(&mut map, now),
        }
    }

    /// Records one failure for `key`.
    pub fn record_failure(&self, key: &str, now: Instant) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        if !map.contains_key(key) && map.len() >= self.capacity && !self.make_room(&mut map, now) {
            // Full of live keys: `challenge_required` already fails closed
            // for keys it cannot track.
            return;
        }
        let list = map.entry(key.to_string()).or_default();
        self.prune(list, now);
        list.push_back(now);
        // A key never needs more entries than any sane threshold.
        while list.len() > 64 {
            list.pop_front();
        }
    }

    /// Drops expired keys; `true` if there is now room for one more.
    fn make_room(&self, map: &mut HashMap<String, VecDeque<Instant>>, now: Instant) -> bool {
        map.retain(|_, list| {
            self.prune(list, now);
            !list.is_empty()
        });
        map.len() < self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_window_and_no_reset() {
        let c = FailureCounter::new(Duration::from_secs(60), 10);
        let t0 = Instant::now();
        let k = failure_key("t1", "auth/login", "1.2.3.4");
        assert!(!c.challenge_required(&k, 3, t0));
        c.record_failure(&k, t0);
        c.record_failure(&k, t0);
        assert!(!c.challenge_required(&k, 3, t0));
        c.record_failure(&k, t0 + Duration::from_secs(10));
        assert!(c.challenge_required(&k, 3, t0 + Duration::from_secs(10)));
        // Other IP, route and tenant are separate.
        assert!(!c.challenge_required(&failure_key("t1", "auth/login", "1.2.3.5"), 3, t0));
        assert!(!c.challenge_required(&failure_key("t2", "auth/login", "1.2.3.4"), 3, t0));
        // The two oldest leave the window.
        assert!(!c.challenge_required(&k, 3, t0 + Duration::from_secs(61)));
        assert!(c.challenge_required(&k, 1, t0 + Duration::from_secs(61)));
        assert!(!c.challenge_required(&k, 1, t0 + Duration::from_secs(71)));
    }

    #[test]
    fn zero_threshold_always_challenges() {
        let c = FailureCounter::default();
        assert!(c.challenge_required("k", 0, Instant::now()));
    }

    #[test]
    fn full_store_fails_closed_until_keys_expire() {
        let c = FailureCounter::new(Duration::from_secs(60), 2);
        let t0 = Instant::now();
        c.record_failure("a", t0);
        c.record_failure("b", t0);
        assert!(
            c.challenge_required("c", 3, t0),
            "untracked key in a full store"
        );
        assert!(!c.challenge_required("a", 3, t0));
        c.record_failure("c", t0);
        assert!(c.challenge_required("c", 3, t0));
        let later = t0 + Duration::from_secs(61);
        assert!(
            !c.challenge_required("c", 3, later),
            "expired keys make room"
        );
    }
}
