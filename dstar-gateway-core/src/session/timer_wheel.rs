//! Deterministic timer wheel keyed by `Instant`.
//!
//! Stores N named timers. Each timer has a target instant. The
//! wheel tells the shell what its next wake-up time is via
//! `next_deadline`. When time advances past a timer's target, the
//! caller (the session machine) checks `is_expired(name, now)`.

use std::collections::HashMap;
use std::time::Instant;

/// Named timer identifier — a `&'static str` keeps the wheel
/// allocation-free for fixed sets of timers (keepalive,
/// inactivity, connect-deadline, voice-inactivity, etc.).
pub(crate) type TimerId = &'static str;

/// Tiny timer wheel for the session machines.
#[derive(Debug, Default)]
pub(crate) struct TimerWheel {
    timers: HashMap<TimerId, Instant>,
}

impl TimerWheel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Set or reset a named timer.
    pub(crate) fn set(&mut self, name: TimerId, deadline: Instant) {
        let _previous = self.timers.insert(name, deadline);
    }

    /// Remove a named timer.
    pub(crate) fn clear(&mut self, name: TimerId) {
        let _previous = self.timers.remove(name);
    }

    /// True if `now >= timer.deadline`.
    pub(crate) fn is_expired(&self, name: TimerId, now: Instant) -> bool {
        self.timers.get(name).is_some_and(|&d| now >= d)
    }

    /// Earliest unexpired deadline across all timers, or `None` if
    /// no timers are set.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.timers.values().copied().min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_wheel_has_no_deadline() {
        let w = TimerWheel::new();
        assert!(w.next_deadline().is_none());
    }

    #[test]
    fn set_and_check_expiry() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        w.set("keepalive", now + Duration::from_secs(1));
        assert!(!w.is_expired("keepalive", now));
        assert!(w.is_expired("keepalive", now + Duration::from_secs(2)));
    }

    #[test]
    fn next_deadline_is_earliest() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        w.set("a", now + Duration::from_secs(5));
        w.set("b", now + Duration::from_secs(2));
        w.set("c", now + Duration::from_secs(10));
        assert_eq!(w.next_deadline(), Some(now + Duration::from_secs(2)));
    }

    #[test]
    fn clear_removes_timer() {
        let mut w = TimerWheel::new();
        let now = Instant::now();
        w.set("a", now + Duration::from_secs(1));
        w.clear("a");
        assert!(w.next_deadline().is_none());
    }
}
