//! Reconnect backoff policy.

use std::time::Duration;

/// Default initial reconnect delay.
const DEFAULT_RECONNECT_INITIAL: Duration = Duration::from_secs(1);

/// Default reconnect delay ceiling.
const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Why a reconnect backoff policy could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReconnectPolicyError {
    /// The first delay is longer than the configured ceiling.
    #[error(
        "initial reconnect delay {initial_delay:?} exceeds maximum reconnect delay {maximum_delay:?}"
    )]
    InitialDelayExceedsMaximum {
        /// Requested delay before the first reconnect attempt.
        initial_delay: Duration,
        /// Requested reconnect-delay ceiling.
        maximum_delay: Duration,
    },
}

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Initial delay before the first retry.
    initial_delay: Duration,
    /// Maximum delay between retries.
    maximum_delay: Duration,
    /// Delay that the next call to [`Self::next_delay`] returns.
    current_delay: Duration,
    /// Number of reconnect delays issued since the last reset.
    attempts: u32,
}

impl ReconnectPolicy {
    /// Creates a policy with custom initial and maximum delays.
    ///
    /// Zero-length delays are valid. The initial delay must not exceed the
    /// maximum because a policy must never return a delay above its ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`ReconnectPolicyError::InitialDelayExceedsMaximum`] when
    /// `initial_delay` is longer than `maximum_delay`.
    pub const fn new(
        initial_delay: Duration,
        maximum_delay: Duration,
    ) -> Result<Self, ReconnectPolicyError> {
        if initial_delay.as_nanos() > maximum_delay.as_nanos() {
            return Err(ReconnectPolicyError::InitialDelayExceedsMaximum {
                initial_delay,
                maximum_delay,
            });
        }

        Ok(Self {
            initial_delay,
            maximum_delay,
            current_delay: initial_delay,
            attempts: 0,
        })
    }

    /// Returns the delay before the first reconnect attempt.
    #[must_use]
    pub const fn initial_delay(&self) -> Duration {
        self.initial_delay
    }

    /// Returns the maximum delay between reconnect attempts.
    #[must_use]
    pub const fn maximum_delay(&self) -> Duration {
        self.maximum_delay
    }

    /// Returns the next delay and advances the backoff state.
    ///
    /// The delay doubles with each call, up to [`Self::maximum_delay`]. If
    /// doubling would overflow [`Duration`], the next delay becomes the
    /// configured maximum instead. The returned delay is always within the
    /// validated inclusive range from the initial delay through the maximum.
    #[must_use]
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current_delay;
        self.attempts = self.attempts.saturating_add(1);
        self.current_delay = self
            .current_delay
            .checked_mul(2)
            .unwrap_or(self.maximum_delay)
            .min(self.maximum_delay);
        delay
    }

    /// Reset the backoff state after a successful connection.
    pub const fn reset(&mut self) {
        self.current_delay = self.initial_delay;
        self.attempts = 0;
    }

    /// Returns the number of reconnect attempts scheduled since the last
    /// reset.
    ///
    /// [`Self::next_delay`] schedules one attempt by returning its delay and
    /// increments this count. The count saturates at [`u32::MAX`] rather than
    /// wrapping if the state machine is advanced that many times without a
    /// reset.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: DEFAULT_RECONNECT_INITIAL,
            maximum_delay: DEFAULT_RECONNECT_MAX,
            current_delay: DEFAULT_RECONNECT_INITIAL,
            attempts: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_policy_exponential_backoff() {
        let mut policy = ReconnectPolicy::default();
        let first_delay = policy.next_delay();
        let second_delay = policy.next_delay();
        assert_eq!(
            first_delay, DEFAULT_RECONNECT_INITIAL,
            "the first delay must equal the configured initial delay"
        );
        assert_eq!(
            second_delay,
            DEFAULT_RECONNECT_INITIAL * 2,
            "the second delay must double the initial delay"
        );
    }

    #[test]
    fn reconnect_policy_caps_at_max() -> Result<(), ReconnectPolicyError> {
        let mut policy = ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(4))?;
        for expected_delay in [1, 2, 4, 4].map(Duration::from_secs) {
            assert_eq!(
                policy.next_delay(),
                expected_delay,
                "the delay sequence must double and then remain at the ceiling"
            );
        }
        Ok(())
    }

    #[test]
    fn reconnect_policy_reset() {
        let mut policy = ReconnectPolicy::default();
        let _ = policy.next_delay();
        let _ = policy.next_delay();
        assert!(policy.attempts() > 0, "attempts should have advanced");
        policy.reset();
        assert_eq!(
            policy.attempts(),
            0,
            "reset must clear the consecutive-attempt count"
        );
        assert_eq!(
            policy.next_delay(),
            policy.initial_delay(),
            "the first delay after reset must be the configured initial delay"
        );
    }

    #[test]
    fn reconnect_policy_rejects_an_initial_delay_above_its_ceiling() {
        let initial_delay = Duration::from_secs(2);
        let maximum_delay = Duration::from_secs(1);

        assert_eq!(
            ReconnectPolicy::new(initial_delay, maximum_delay),
            Err(ReconnectPolicyError::InitialDelayExceedsMaximum {
                initial_delay,
                maximum_delay,
            }),
            "an invalid delay order must be reported exactly"
        );
    }

    #[test]
    fn reconnect_policy_accepts_equal_delays() -> Result<(), ReconnectPolicyError> {
        let delay = Duration::from_secs(1);
        let mut policy = ReconnectPolicy::new(delay, delay)?;

        assert_eq!(
            policy.initial_delay(),
            delay,
            "the policy must preserve its initial delay"
        );
        assert_eq!(
            policy.maximum_delay(),
            delay,
            "the policy must preserve its maximum delay"
        );
        assert_eq!(
            policy.next_delay(),
            delay,
            "the first delay may equal the ceiling"
        );
        assert_eq!(
            policy.next_delay(),
            delay,
            "equal initial and maximum delays must remain constant"
        );
        Ok(())
    }

    #[test]
    fn reconnect_policy_duration_overflow_uses_the_ceiling() -> Result<(), ReconnectPolicyError> {
        let mut policy = ReconnectPolicy::new(Duration::MAX, Duration::MAX)?;

        assert_eq!(
            policy.next_delay(),
            Duration::MAX,
            "the first maximum-duration delay must be returned exactly"
        );
        assert_eq!(
            policy.next_delay(),
            Duration::MAX,
            "doubling overflow must leave the next delay at the ceiling"
        );
        Ok(())
    }

    #[test]
    fn reconnect_policy_attempt_count_saturates() {
        let mut policy = ReconnectPolicy {
            attempts: u32::MAX,
            ..ReconnectPolicy::default()
        };

        let _ = policy.next_delay();
        assert_eq!(
            policy.attempts(),
            u32::MAX,
            "the consecutive-attempt count must saturate instead of wrapping"
        );
    }
}
