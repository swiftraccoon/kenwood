//! Reconnect backoff policy.

use std::time::Duration;

/// Default initial reconnect delay.
const DEFAULT_RECONNECT_INITIAL: Duration = Duration::from_secs(1);

/// Default reconnect delay ceiling.
const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Exponential backoff policy for link reconnection.
///
/// Provides a state machine that tracks reconnection attempts and
/// computes the next delay using exponential backoff with a configurable
/// ceiling.
///
/// # Usage
///
/// ```
/// use kenwood_thd75::session::ReconnectPolicy;
///
/// let mut policy = ReconnectPolicy::default();
/// // After first failure:
/// let delay = policy.next_delay();
/// // ... wait `delay`, then retry ...
/// // On success:
/// policy.reset();
/// ```
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Initial delay before the first retry.
    pub initial_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
    /// Current delay (doubles after each failure).
    current_delay: Duration,
    /// Number of consecutive failures.
    attempts: u32,
}

impl ReconnectPolicy {
    /// Create a new policy with custom initial and max delays.
    #[must_use]
    pub const fn new(initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            initial_delay,
            max_delay,
            current_delay: initial_delay,
            attempts: 0,
        }
    }

    /// Get the next delay and advance the backoff state.
    ///
    /// The delay doubles with each call, up to `max_delay`.
    #[must_use]
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current_delay;
        self.attempts = self.attempts.saturating_add(1);
        self.current_delay = (self.current_delay * 2).min(self.max_delay);
        delay
    }

    /// Reset the backoff state after a successful connection.
    pub const fn reset(&mut self) {
        self.current_delay = self.initial_delay;
        self.attempts = 0;
    }

    /// Number of consecutive reconnection attempts.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_RECONNECT_INITIAL, DEFAULT_RECONNECT_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_policy_exponential_backoff() {
        let mut policy = ReconnectPolicy::default();
        let d1 = policy.next_delay();
        let d2 = policy.next_delay();
        assert_eq!(d1, DEFAULT_RECONNECT_INITIAL);
        assert_eq!(d2, DEFAULT_RECONNECT_INITIAL * 2);
    }

    #[test]
    fn reconnect_policy_caps_at_max() {
        let mut policy = ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(4));
        for _ in 0..10 {
            let d = policy.next_delay();
            assert!(d <= Duration::from_secs(4), "delay capped at max");
        }
    }

    #[test]
    fn reconnect_policy_reset() {
        let mut policy = ReconnectPolicy::default();
        let _ = policy.next_delay();
        let _ = policy.next_delay();
        assert!(policy.attempts() > 0, "attempts should have advanced");
        policy.reset();
        assert_eq!(policy.attempts(), 0);
    }
}
