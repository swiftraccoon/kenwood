//! Per-client protocol state and an injected-time recipient token bucket.

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Instant;

use dstar_gateway_core::ServerSessionCore;
use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::types::Module;

use crate::reflector::AccessPolicy;

use super::FrameRate;

/// Default one-second recipient burst capacity, in frames.
pub const DEFAULT_TX_BUDGET_MAX_TOKENS: u32 = 60;

/// Default recipient refill rate, 60 frames/s.
///
/// This local policy gives headroom above the 50-frame/s voice cadence;
/// arrivals exceeding the configured burst/rate can still be dropped.
pub const DEFAULT_TX_BUDGET_REFILL_PER_SEC: FrameRate = match FrameRate::new(60.0) {
    Ok(rate) => rate,
    Err(_) => unreachable!(),
};

/// Injected-time token bucket for one recipient's outbound frame budget.
///
/// Starts full, refills at a checked rate up to its capacity, and withdraws
/// only when sufficient tokens exist. Zero capacity permits only zero-cost
/// operations. Regressing input time never moves the refill timestamp back.
///
/// ```
/// use std::time::{Duration, Instant};
/// use dstar_gateway_server::{FrameRate, TokenBucket};
/// let start = Instant::now();
/// let mut budget = TokenBucket::new(2, FrameRate::new(1.0)?, start);
/// assert!(budget.try_consume(start, 2));
/// assert!(!budget.try_consume(start, 1));
/// assert!(budget.try_consume(start + Duration::from_secs(1), 1));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate_per_sec: FrameRate,
    last_refill: Instant,
}

impl TokenBucket {
    /// Start with `max_tokens` available at the given injected instant.
    #[must_use]
    pub fn new(max_tokens: u32, refill_rate_per_sec: FrameRate, now: Instant) -> Self {
        let max_tokens = f64::from(max_tokens);
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate_per_sec,
            last_refill: now,
        }
    }

    /// Withdraw a frame cost after refilling through now.
    ///
    /// Returns false without withdrawing when the balance is insufficient.
    /// Clock regression contributes no refill and preserves the prior timestamp.
    pub fn try_consume(&mut self, now: Instant, tokens: u32) -> bool {
        if now > self.last_refill {
            let elapsed = now.duration_since(self.last_refill).as_secs_f64();
            let refill = elapsed * self.refill_rate_per_sec.get();
            self.tokens = (self.tokens + refill).min(self.max_tokens);
            self.last_refill = now;
        }
        let cost = f64::from(tokens);
        if self.tokens < cost {
            return false;
        }
        self.tokens -= cost;
        true
    }

    /// Use one second of configured traffic as capacity, with at least one frame.
    ///
    /// Fractional rates below one frame/s therefore permit an initial single
    /// frame, then refill at their actual configured rate.
    #[must_use]
    pub const fn from_rate(rate_per_sec: FrameRate, now: Instant) -> Self {
        let max_tokens = rate_per_sec.get().max(1.0);
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate_per_sec: rate_per_sec,
            last_refill: now,
        }
    }

    /// Current token balance after the last attempted withdrawal.
    #[must_use]
    pub const fn tokens(&self) -> f64 {
        self.tokens
    }

    /// Maximum token balance.
    #[must_use]
    pub const fn max_tokens(&self) -> f64 {
        self.max_tokens
    }

    /// Validated refill rate in frames per second.
    #[must_use]
    pub const fn refill_rate_per_sec(&self) -> FrameRate {
        self.refill_rate_per_sec
    }
}

/// One uniquely owned protocol client tracked by a pool.
///
/// Construction derives the core protocol from P and binds the same peer
/// address used by the pool. Public observations cannot mutate indexed fields.
#[derive(Debug)]
pub(crate) struct ClientHandle<P> {
    pub(crate) session: ServerSessionCore,
    pub(crate) module: Option<Module>,
    pub(crate) last_heard: Instant,
    pub(crate) access: AccessPolicy,
    pub(crate) send_failure_count: u32,
    pub(crate) tx_budget: TokenBucket,
    _protocol: PhantomData<fn() -> P>,
}

impl<P: Protocol> ClientHandle<P> {
    /// Create an unlinked client for P using the default recipient budget.
    #[must_use]
    pub(crate) fn new(
        peer: SocketAddr,
        default_module: Module,
        access: AccessPolicy,
        now: Instant,
    ) -> Self {
        Self {
            session: ServerSessionCore::new(P::KIND, peer, default_module),
            module: None,
            last_heard: now,
            access,
            send_failure_count: 0,
            tx_budget: TokenBucket::new(
                DEFAULT_TX_BUDGET_MAX_TOKENS,
                DEFAULT_TX_BUDGET_REFILL_PER_SEC,
                now,
            ),
            _protocol: PhantomData,
        }
    }

    /// Replace the recipient budget before transferring this handle to a pool.
    #[must_use]
    pub(crate) const fn with_tx_budget(mut self, budget: TokenBucket) -> Self {
        self.tx_budget = budget;
        self
    }

    /// Peer address fixed at construction.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn peer(&self) -> SocketAddr {
        self.session.peer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const ONE: FrameRate = match FrameRate::new(1.0) {
        Ok(rate) => rate,
        Err(_) => unreachable!(),
    };

    #[test]
    fn capacity_and_elapsed_refill_bound_withdrawals() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(2, ONE, start);
        assert!(
            bucket.try_consume(start, 2),
            "initial capacity is available"
        );
        assert!(
            !bucket.try_consume(start, 1),
            "same-instant overdraft is refused"
        );
        let half = start + Duration::from_millis(500);
        assert!(
            !bucket.try_consume(half, 1),
            "fractional refill is insufficient"
        );
        assert!(
            bucket.try_consume(start + Duration::from_secs(1), 1),
            "one elapsed second refills one"
        );
        assert!(
            bucket.try_consume(start + Duration::from_secs(100), 2),
            "refill caps at capacity"
        );
        assert!(
            !bucket.try_consume(start + Duration::from_secs(100), 1),
            "no extra overflow credit"
        );
    }

    #[test]
    fn rewinding_time_cannot_refill_the_same_interval_twice() {
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        let mut bucket = TokenBucket::new(1, ONE, start);
        assert!(bucket.try_consume(start, 1));
        assert!(bucket.try_consume(later, 1));
        assert!(
            bucket.try_consume(start, 0),
            "zero cost remains valid during regression"
        );
        assert!(
            !bucket.try_consume(later, 1),
            "previously credited time cannot be reused"
        );
    }

    #[test]
    fn zero_capacity_and_fractional_rate_have_explicit_behavior()
    -> Result<(), Box<dyn std::error::Error>> {
        let start = Instant::now();
        let mut disabled = TokenBucket::new(0, ONE, start);
        assert!(disabled.try_consume(start, 0));
        assert!(!disabled.try_consume(start + Duration::from_secs(1), 1));
        let mut slow = TokenBucket::from_rate(FrameRate::new(0.05)?, start);
        assert!(
            slow.try_consume(start, 1),
            "sub-one rate permits one initial frame"
        );
        assert!(!slow.try_consume(start + Duration::from_secs(1), 1));
        assert!(
            slow.try_consume(start + Duration::from_secs(20), 1),
            "fractional rate remains effective"
        );
        Ok(())
    }
}
