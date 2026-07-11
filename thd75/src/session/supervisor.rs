//! Opt-in link supervisor for a [`Radio`].
//!
//! Wraps a [`Radio`] and drives [`Radio::reconnect`] under a
//! [`ReconnectPolicy`] when the link drops, broadcasting typed
//! [`LinkEvent`]s for status displays. Commands still flow through one
//! exclusive handle ([`RadioSupervisor::radio`]) and nothing is ever
//! replayed on the caller's behalf.

use std::time::Duration;

use crate::error::Error;
use crate::radio::{LinkState, Radio};
use crate::session::ReconnectPolicy;
use crate::transport::Transport;

/// Broadcast capacity for [`LinkEvent`]s. Slow subscribers that fall
/// further behind than this lag (tokio broadcast semantics) skip old
/// events rather than blocking the supervisor.
const EVENT_CAPACITY: usize = 16;

/// A link-supervision event broadcast by [`RadioSupervisor::heal`].
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

/// Opt-in supervisor that heals a dropped radio link with backoff.
///
/// The supervisor owns the [`Radio`]; commands flow through
/// [`radio`](Self::radio) so there is exactly one command path.
/// Healing is explicitly driven: call [`heal`](Self::heal) after a
/// command fails or when [`Radio::link_state`] reports
/// [`LinkState::Down`].
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use kenwood_thd75::radio::Radio;
/// use kenwood_thd75::session::{RadioSupervisor, ReconnectPolicy};
/// use kenwood_thd75::transport::SerialTransport;
///
/// let port = SerialTransport::open("/dev/cu.usbmodem1101", SerialTransport::DEFAULT_BAUD)?;
/// let radio = Radio::connect(port).await?;
/// let mut sup = RadioSupervisor::new(radio, ReconnectPolicy::default(), 5);
/// let mut events = sup.events();
///
/// if sup.radio().get_firmware_version().await.is_err() {
///     sup.heal().await?;
/// }
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct RadioSupervisor<T: Transport> {
    radio: Radio<T>,
    policy: ReconnectPolicy,
    max_attempts: u32,
    events_tx: tokio::sync::broadcast::Sender<LinkEvent>,
}

impl<T: Transport> RadioSupervisor<T> {
    /// Wrap `radio` with a reconnect `policy`.
    ///
    /// `max_attempts` bounds one [`heal`](Self::heal) run; the policy
    /// itself only shapes the delays. Zero is clamped to one attempt.
    #[must_use]
    pub fn new(radio: Radio<T>, policy: ReconnectPolicy, max_attempts: u32) -> Self {
        let (events_tx, _) = tokio::sync::broadcast::channel(EVENT_CAPACITY);
        Self {
            radio,
            policy,
            max_attempts: max_attempts.max(1),
            events_tx,
        }
    }

    /// Subscribe to link-supervision events.
    #[must_use]
    pub fn events(&self) -> tokio::sync::broadcast::Receiver<LinkEvent> {
        self.events_tx.subscribe()
    }

    /// The supervised radio — the one and only command path.
    pub const fn radio(&mut self) -> &mut Radio<T> {
        &mut self.radio
    }

    /// Give the radio back, dropping supervision.
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
    /// # Errors
    ///
    /// The final reconnect error when every allowed attempt failed.
    pub async fn heal(&mut self) -> Result<(), Error> {
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
                Err(e) if attempt >= self.max_attempts => {
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
        // No subscribers is fine — events are advisory.
        let _ = self.events_tx.send(event);
    }
}
