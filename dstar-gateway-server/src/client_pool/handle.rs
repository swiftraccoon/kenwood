//! `ClientHandle` — one linked client as tracked by the reflector.
//!
//! The pool stores protocol-erased [`ServerSessionCore`] instances
//! keyed by `SocketAddr`. A phantom `P: Protocol` marker threads the
//! protocol type through the API so callers can get DExtra/DPlus/DCS
//! typed accessors without the storage itself being generic.
//!
//! This module also defines [`TokenBucket`], the per-client rate
//! limiter used to cap the number of fan-out voice frames a single
//! client can consume per second. It lives here (rather than in a
//! dedicated module) to keep the per-handle state co-located.

use std::marker::PhantomData;
use std::time::Instant;

use dstar_gateway_core::ServerSessionCore;
use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::types::Module;

use crate::reflector::AccessPolicy;

/// Default burst capacity for per-client TX rate limiting, in frames.
///
/// Sized to absorb a ~1 second burst of voice at the nominal 20 fps
/// D-STAR rate plus headroom.
pub const DEFAULT_TX_BUDGET_MAX_TOKENS: u32 = 60;

/// Default steady-state refill rate for per-client TX rate limiting,
/// in frames per second.
///
/// Set to `60.0` (3× the nominal 20 fps D-STAR voice rate) to leave
/// headroom for jitter and burstiness. A client legitimately
/// transmitting audio will never hit the limit; a client trying to
/// `DoS` the reflector with 200 fps of voice will.
pub const DEFAULT_TX_BUDGET_REFILL_PER_SEC: f64 = 60.0;

/// Rate limiter that caps the number of tokens consumed per second.
///
/// Used on [`ClientHandle`] to throttle how many fan-out voice
/// frames a single client can absorb per second, so one slow or
/// adversarial client can't monopolize the reflector's fan-out
/// loop. Classic leaky/token-bucket hybrid: the bucket starts full
/// and [`Self::try_consume`] refills by `refill_rate_per_sec *
/// elapsed` tokens (capped at `max_tokens`) before attempting to
/// withdraw.
///
/// This is a sans-io state machine: [`Self::try_consume`] takes
/// the caller's current [`Instant`] and never calls
/// [`Instant::now`] itself. Tests drive the clock forward by
/// constructing synthetic instants.
#[derive(Debug, Clone, Copy)]
pub struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate_per_sec: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Construct a new bucket full of tokens.
    ///
    /// - `max_tokens`: burst capacity; the bucket starts with this
    ///   many tokens and refills up to (but never beyond) this
    ///   value.
    /// - `refill_rate_per_sec`: steady-state refill rate in tokens
    ///   per second. A nominal 20 fps D-STAR voice stream would set
    ///   this to `60.0` (3× nominal) to leave headroom for jitter.
    /// - `now`: the wall-clock instant at which the bucket is
    ///   constructed, used as the seed for the first refill delta.
    #[must_use]
    pub fn new(max_tokens: u32, refill_rate_per_sec: f64, now: Instant) -> Self {
        let max_tokens = f64::from(max_tokens);
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate_per_sec,
            last_refill: now,
        }
    }

    /// Attempt to withdraw `tokens` from the bucket at time `now`.
    ///
    /// Returns `true` if the bucket had enough tokens (withdrawal
    /// committed); returns `false` otherwise.
    pub fn try_consume(&mut self, now: Instant, tokens: u32) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        let refill = elapsed.as_secs_f64() * self.refill_rate_per_sec;
        self.tokens = (self.tokens + refill).min(self.max_tokens);
        self.last_refill = now;

        let cost = f64::from(tokens);
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }

    /// Current (refill-approximated) token count, for tests/metrics.
    #[must_use]
    pub const fn tokens(&self) -> f64 {
        self.tokens
    }

    /// Maximum tokens the bucket can hold.
    #[must_use]
    pub const fn max_tokens(&self) -> f64 {
        self.max_tokens
    }

    /// Refill rate in tokens per second.
    #[must_use]
    pub const fn refill_rate_per_sec(&self) -> f64 {
        self.refill_rate_per_sec
    }
}

/// One entry in [`super::ClientPool`].
///
/// Tracks the per-peer server session, its module membership (if
/// any), the last time we heard from the client, the access policy
/// the authorizer granted, a running count of send failures so the
/// fan-out engine can evict unhealthy peers, and a per-client TX
/// token bucket used to rate-limit how many fan-out voice frames
/// one client can consume per second.
#[derive(Debug)]
pub struct ClientHandle<P: Protocol> {
    /// Protocol-erased server session state machine.
    pub session: ServerSessionCore,
    /// Module the client has linked to, if any.
    pub module: Option<Module>,
    /// Last time we received a datagram from this client.
    pub last_heard: Instant,
    /// Access policy granted by the authorizer.
    pub access: AccessPolicy,
    /// Monotonically increasing count of fan-out send failures.
    pub send_failure_count: u32,
    /// Per-client TX token bucket. Each outbound voice frame in
    /// fan-out consumes one token; when the bucket is empty, the
    /// frame is dropped for THIS peer (the other peers on the same
    /// module still receive it). Rate-limited is NOT the same as
    /// broken — the peer is not marked unhealthy.
    pub tx_budget: TokenBucket,
    _protocol: PhantomData<fn() -> P>,
}

impl<P: Protocol> ClientHandle<P> {
    /// Construct a new handle for a freshly observed client.
    ///
    /// The TX budget is initialized with [`DEFAULT_TX_BUDGET_MAX_TOKENS`]
    /// capacity and [`DEFAULT_TX_BUDGET_REFILL_PER_SEC`] refill rate.
    #[must_use]
    pub fn new(session: ServerSessionCore, access: AccessPolicy, now: Instant) -> Self {
        Self::new_with_tx_budget(
            session,
            access,
            now,
            DEFAULT_TX_BUDGET_MAX_TOKENS,
            DEFAULT_TX_BUDGET_REFILL_PER_SEC,
        )
    }

    /// Construct a new handle with a caller-specified TX budget.
    ///
    /// Primarily used by tests that need to drive the rate limiter
    /// past its limit in a single tick without waiting for real
    /// wall-clock refill.
    #[must_use]
    pub fn new_with_tx_budget(
        session: ServerSessionCore,
        access: AccessPolicy,
        now: Instant,
        max_tokens: u32,
        refill_rate_per_sec: f64,
    ) -> Self {
        Self {
            session,
            module: None,
            last_heard: now,
            access,
            send_failure_count: 0,
            tx_budget: TokenBucket::new(max_tokens, refill_rate_per_sec, now),
            _protocol: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TokenBucket;
    use std::time::{Duration, Instant};

    #[test]
    fn bucket_starts_full() {
        let now = Instant::now();
        let bucket = TokenBucket::new(5, 1.0, now);
        assert!((bucket.tokens() - 5.0).abs() < 1e-9);
        assert!((bucket.max_tokens() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn consume_under_budget_succeeds() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(3, 1.0, now);
        assert!(bucket.try_consume(now, 1));
        assert!(bucket.try_consume(now, 1));
        assert!(bucket.try_consume(now, 1));
    }

    #[test]
    fn consume_over_budget_fails() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(1, 1.0, now);
        assert!(bucket.try_consume(now, 1), "first consume succeeds");
        assert!(!bucket.try_consume(now, 1), "empty bucket rejects");
    }

    #[test]
    fn bucket_refills_over_time() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(1, 1.0, start);
        assert!(bucket.try_consume(start, 1), "first consume");
        assert!(!bucket.try_consume(start, 1), "second fails at t0");
        let t1 = start + Duration::from_secs(1);
        assert!(bucket.try_consume(t1, 1), "refilled to 1 after 1s");
    }

    #[test]
    fn bucket_refill_clamps_to_max_tokens() {
        let start = Instant::now();
        let mut bucket = TokenBucket::new(2, 10.0, start);
        assert!(bucket.try_consume(start, 2));
        let t1 = start + Duration::from_secs(10);
        assert!(bucket.try_consume(t1, 2), "bucket refills to max");
        assert!(!bucket.try_consume(t1, 1), "bucket empty again");
    }

    #[test]
    fn consume_zero_tokens_always_succeeds() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new(0, 0.0, now);
        assert!(bucket.try_consume(now, 0), "zero cost always succeeds");
    }

    #[test]
    fn regressing_clock_does_not_underflow() {
        let later = Instant::now();
        let mut bucket = TokenBucket::new(1, 1.0, later);
        let earlier = later.checked_sub(Duration::from_secs(1)).unwrap_or(later);
        assert!(bucket.try_consume(earlier, 1));
    }
}
