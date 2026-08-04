//! Explicit link recovery for a [`Radio`].
//!
//! Wraps a [`Radio`] and drives [`Radio::reconnect`] under a
//! [`ReconnectPolicy`] when the link drops, broadcasting typed
//! [`LinkEvent`]s for status displays. Commands still flow through one
//! exclusive handle ([`RadioLinkRecovery::radio`]) and nothing is ever
//! replayed on the caller's behalf.

use std::num::NonZeroU32;
use std::time::Duration;

use crate::error::Error;
use crate::radio::{LinkState, Radio};
use crate::session::ReconnectPolicy;
use crate::transport::Transport;

/// Broadcast capacity for [`LinkEvent`]s. Slow subscribers that fall
/// further behind than this lag (tokio broadcast semantics) skip old
/// events rather than blocking recovery.
const EVENT_CAPACITY: usize = 16;

/// Why a reconnect-attempt limit could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReconnectAttemptLimitError {
    /// Zero would prevent every reconnect attempt.
    #[error("reconnect attempt limit must be at least one")]
    Zero,
}

/// A validated, nonzero limit on reconnect attempts in one healing run.
///
/// The limit is inclusive: a value of three permits attempts 1, 2, and 3.
/// Values through [`u32::MAX`] are representable, and zero is rejected rather
/// than silently changed into a different policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReconnectAttemptLimit(NonZeroU32);

impl ReconnectAttemptLimit {
    /// Creates a nonzero reconnect-attempt limit.
    ///
    /// # Errors
    ///
    /// Returns [`ReconnectAttemptLimitError::Zero`] when `maximum_attempts` is
    /// zero.
    pub const fn new(maximum_attempts: u32) -> Result<Self, ReconnectAttemptLimitError> {
        let Some(maximum_attempts) = NonZeroU32::new(maximum_attempts) else {
            return Err(ReconnectAttemptLimitError::Zero);
        };
        Ok(Self(maximum_attempts))
    }

    /// Returns the inclusive maximum number of attempts.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0.get()
    }
}

impl From<NonZeroU32> for ReconnectAttemptLimit {
    fn from(maximum_attempts: NonZeroU32) -> Self {
        Self(maximum_attempts)
    }
}

impl TryFrom<u32> for ReconnectAttemptLimit {
    type Error = ReconnectAttemptLimitError;

    fn try_from(maximum_attempts: u32) -> Result<Self, Self::Error> {
        Self::new(maximum_attempts)
    }
}

/// A link-recovery event broadcast by [`RadioLinkRecovery::recover`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// The link was observed down; healing is starting.
    Lost,
    /// A reconnect attempt is about to run after the given delay.
    Reconnecting {
        /// 1-based attempt number.
        attempt: u32,
        /// Backoff delay waited before this attempt.
        next_delay: Duration,
    },
    /// The link is healthy again.
    Restored,
    /// Every allowed attempt failed; the caller decides what's next.
    GaveUp {
        /// How many attempts were made.
        attempts: u32,
    },
}

/// Explicit recovery for a dropped radio link with backoff.
///
/// The recovery wrapper owns the [`Radio`]; commands flow through
/// [`radio`](Self::radio) so there is exactly one command path.
/// Recovery is explicitly driven: call [`recover`](Self::recover) after a
/// command fails or when [`Radio::link_state`] reports
/// [`LinkState::Down`].
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use kenwood_thd75::radio::Radio;
/// use kenwood_thd75::session::{
///     RadioLinkRecovery, ReconnectAttemptLimit, ReconnectPolicy,
/// };
/// use kenwood_thd75::transport::SerialTransport;
///
/// let port = SerialTransport::open("/dev/cu.usbmodem1101")?;
/// let radio = Radio::new(port);
/// let attempt_limit = ReconnectAttemptLimit::new(5)?;
/// let mut recovery = RadioLinkRecovery::new(radio, ReconnectPolicy::default(), attempt_limit);
/// let _events = recovery.events();
///
/// if recovery.radio().get_firmware_version().await.is_err() {
///     recovery.recover().await?;
/// }
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct RadioLinkRecovery<T: Transport> {
    radio: Radio<T>,
    policy: ReconnectPolicy,
    attempt_limit: ReconnectAttemptLimit,
    events_tx: tokio::sync::broadcast::Sender<LinkEvent>,
}

impl<T: Transport> RadioLinkRecovery<T> {
    /// Wraps `radio` with a reconnect `policy` and an `attempt_limit`.
    ///
    /// The limit bounds one [`recover`](Self::recover) run; the policy itself
    /// only shapes the delays. Its validated type makes zero-attempt recovery
    /// impossible to construct.
    #[must_use]
    pub fn new(
        radio: Radio<T>,
        policy: ReconnectPolicy,
        attempt_limit: ReconnectAttemptLimit,
    ) -> Self {
        let (events_tx, _) = tokio::sync::broadcast::channel(EVENT_CAPACITY);
        Self {
            radio,
            policy,
            attempt_limit,
            events_tx,
        }
    }

    /// Returns the inclusive reconnect-attempt limit for each healing run.
    #[must_use]
    pub const fn attempt_limit(&self) -> ReconnectAttemptLimit {
        self.attempt_limit
    }

    /// Subscribe to link-recovery events.
    #[must_use]
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<LinkEvent> {
        self.events_tx.subscribe()
    }

    /// The owned radio: the one and only command path.
    pub const fn radio(&mut self) -> &mut Radio<T> {
        &mut self.radio
    }

    /// Give the radio back, dropping the recovery wrapper.
    #[must_use]
    pub fn into_inner(self) -> Radio<T> {
        self.radio
    }

    /// Drive reconnect attempts until the link is restored or the
    /// attempt budget is spent.
    ///
    /// A no-op when the link is already up. Emits [`LinkEvent::Lost`]
    /// once, then per attempt a [`LinkEvent::Reconnecting`] followed by
    /// the backoff sleep and a [`Radio::reconnect`]. Success resets the
    /// policy and emits [`LinkEvent::Restored`]; exhausting the budget
    /// emits [`LinkEvent::GaveUp`] and returns the last error.
    ///
    /// Event attempt numbers are exact, 1-based counts that restart for each
    /// call and never exceed [`Self::attempt_limit`]. The nonzero limit also
    /// guarantees the loop returns on the final failed `u32` attempt before
    /// another increment could overflow.
    ///
    /// # Errors
    ///
    /// The final reconnect error when every allowed attempt failed.
    pub async fn recover(&mut self) -> Result<(), Error> {
        if *self.radio.link_state().borrow() == LinkState::Up {
            return Ok(());
        }
        self.emit(LinkEvent::Lost);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let next_delay = self.policy.next_delay();
            self.emit(LinkEvent::Reconnecting {
                attempt,
                next_delay,
            });
            tokio::time::sleep(next_delay).await;
            match self.radio.reconnect().await {
                Ok(()) => {
                    self.policy.reset();
                    self.emit(LinkEvent::Restored);
                    return Ok(());
                }
                Err(e) if attempt >= self.attempt_limit.as_raw() => {
                    self.emit(LinkEvent::GaveUp { attempts: attempt });
                    return Err(e);
                }
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "reconnect attempt failed");
                }
            }
        }
    }

    fn emit(&self, event: LinkEvent) {
        // No subscribers is fine; events are advisory.
        let _ = self.events_tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_attempt_limit_rejects_zero_exactly() {
        assert_eq!(
            ReconnectAttemptLimit::new(0),
            Err(ReconnectAttemptLimitError::Zero),
            "zero must be rejected rather than clamped"
        );
    }

    #[test]
    fn attempt_limit_preserves_nonzero_values() -> Result<(), ReconnectAttemptLimitError> {
        assert_eq!(
            ReconnectAttemptLimit::new(1)?.as_raw(),
            1,
            "the smallest valid limit must be preserved"
        );
        assert_eq!(
            ReconnectAttemptLimit::new(u32::MAX)?.as_raw(),
            u32::MAX,
            "the largest representable limit must be preserved"
        );
        Ok(())
    }

    #[test]
    fn reconnect_attempt_limit_converts_from_nonzero_without_validation() {
        let limit = ReconnectAttemptLimit::from(NonZeroU32::MIN);
        assert_eq!(
            limit.as_raw(),
            1,
            "conversion from a nonzero integer must preserve its value"
        );
    }
}
