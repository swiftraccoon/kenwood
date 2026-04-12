//! `StreamCache` — tracks one active voice stream per module.
//!
//! Voice headers must be periodically rebroadcast so clients who
//! joined mid-stream (or missed the initial header) can still decode
//! the audio. The reflector stores one [`StreamCache`] per module
//! currently carrying a stream, and consults it on each incoming
//! frame to decide:
//!
//! - whether to rebroadcast the cached header (every 21 frames, matching
//!   the xlxd cadence in `cdplusprotocol.cpp:318`);
//! - whether the stream has gone silent and should be evicted.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::types::StreamId;

/// Cached state for one active voice stream on one module.
#[derive(Debug, Clone)]
pub struct StreamCache {
    stream_id: StreamId,
    header: DStarHeader,
    /// Raw wire-format bytes of the voice header packet, cached for
    /// 21-frame retransmit. Populated when the endpoint observes a
    /// fresh voice header; re-sent verbatim on each cadence tick.
    header_bytes: Vec<u8>,
    seq_counter: u32,
    started_at: Instant,
    last_activity: Instant,
    from: SocketAddr,
}

impl StreamCache {
    /// Construct a new stream cache entry.
    ///
    /// Called when the reflector sees a fresh voice header from a
    /// client. `now` is the wall-clock instant of receipt, used as
    /// both `started_at` and the initial `last_activity`.
    #[must_use]
    pub const fn new(
        stream_id: StreamId,
        header: DStarHeader,
        from: SocketAddr,
        now: Instant,
    ) -> Self {
        Self {
            stream_id,
            header,
            header_bytes: Vec::new(),
            seq_counter: 0,
            started_at: now,
            last_activity: now,
            from,
        }
    }

    /// Construct a new stream cache entry with the raw header bytes
    /// cached for retransmit.
    ///
    /// Preferred entry point for the fan-out engine, which needs to
    /// re-send the original wire-format header verbatim every 21
    /// frames without re-encoding it.
    #[must_use]
    pub const fn new_with_bytes(
        stream_id: StreamId,
        header: DStarHeader,
        header_bytes: Vec<u8>,
        from: SocketAddr,
        now: Instant,
    ) -> Self {
        Self {
            stream_id,
            header,
            header_bytes,
            seq_counter: 0,
            started_at: now,
            last_activity: now,
            from,
        }
    }

    /// Raw wire-format bytes of the cached voice header.
    ///
    /// Returns an empty slice if the cache was constructed via
    /// [`Self::new`] rather than [`Self::new_with_bytes`].
    #[must_use]
    pub fn header_bytes(&self) -> &[u8] {
        &self.header_bytes
    }

    /// Record the arrival of another voice frame.
    ///
    /// Increments the internal sequence counter and refreshes
    /// `last_activity` so the inactivity watchdog stays armed.
    pub const fn record_frame(&mut self, now: Instant) {
        self.seq_counter = self.seq_counter.saturating_add(1);
        self.last_activity = now;
    }

    /// Whether the cached header should be rebroadcast on the next tick.
    ///
    /// Returns `true` once every 21 frames, matching the xlxd /
    /// `MMDVMHost` cadence. The boundary is `(seq_counter + 1) % 21 == 0`
    /// so the first rebroadcast happens after 20 data frames.
    #[must_use]
    pub const fn should_rebroadcast_header(&self) -> bool {
        (self.seq_counter.wrapping_add(1)).is_multiple_of(21)
    }

    /// Whether this stream has been idle long enough to be evicted.
    #[must_use]
    pub fn should_evict(&self, now: Instant, timeout: Duration) -> bool {
        now.duration_since(self.last_activity) >= timeout
    }

    /// The cached voice header.
    #[must_use]
    pub const fn header(&self) -> &DStarHeader {
        &self.header
    }

    /// The stream id this cache tracks.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    /// The peer that originated the stream (so fan-out can avoid echo).
    #[must_use]
    pub const fn from(&self) -> SocketAddr {
        self.from
    }

    /// When the stream first began (useful for duration metrics).
    #[must_use]
    pub const fn started_at(&self) -> Instant {
        self.started_at
    }

    /// When the last frame was observed (useful for watchdogs / metrics).
    #[must_use]
    pub const fn last_activity(&self) -> Instant {
        self.last_activity
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, Instant, SocketAddr, StreamCache, StreamId};
    use dstar_gateway_core::header::DStarHeader;
    use dstar_gateway_core::types::{Callsign, Suffix};
    use std::net::{IpAddr, Ipv4Addr};

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    fn peer() -> SocketAddr {
        PEER
    }

    fn header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    const fn sid() -> StreamId {
        match StreamId::new(0x1234) {
            Some(s) => s,
            None => unreachable!(),
        }
    }

    #[test]
    fn accessors_return_constructor_inputs() {
        let now = Instant::now();
        let cache = StreamCache::new(sid(), header(), peer(), now);
        assert_eq!(cache.stream_id(), sid());
        assert_eq!(cache.from(), peer());
        assert_eq!(cache.started_at(), now);
        assert_eq!(cache.last_activity(), now);
        // header() returns a borrowed reference — compare field-by-field
        let h = cache.header();
        assert_eq!(h.my_call, header().my_call);
    }

    #[test]
    fn should_rebroadcast_header_fires_every_21_frames() {
        let now = Instant::now();
        let mut cache = StreamCache::new(sid(), header(), peer(), now);
        // First 19 frames: counter becomes 1..=19, (n+1)%21 != 0 until n=20.
        let mut rebroadcasts = 0_u32;
        for _ in 0..50 {
            cache.record_frame(now);
            if cache.should_rebroadcast_header() {
                rebroadcasts = rebroadcasts.saturating_add(1);
            }
        }
        // Two full cycles of 21 within 50 frames → 2 rebroadcasts
        // (at seq_counter == 20 and seq_counter == 41).
        assert_eq!(
            rebroadcasts, 2,
            "two rebroadcast boundaries in 50 frames at 21-frame cadence"
        );
    }

    #[test]
    fn should_rebroadcast_header_is_false_initially() {
        let now = Instant::now();
        let cache = StreamCache::new(sid(), header(), peer(), now);
        // seq_counter=0, (0+1)%21 = 1 != 0
        assert!(!cache.should_rebroadcast_header());
    }

    #[test]
    fn should_evict_triggers_after_timeout() {
        let start = Instant::now();
        let mut cache = StreamCache::new(sid(), header(), peer(), start);
        let timeout = Duration::from_secs(2);
        // Fresh cache — not yet evicted at start.
        assert!(!cache.should_evict(start, timeout));
        // Record a frame at start + 500ms.
        let t1 = start + Duration::from_millis(500);
        cache.record_frame(t1);
        assert!(!cache.should_evict(t1, timeout));
        // 2.5s after last activity → evict.
        let t2 = t1 + Duration::from_millis(2500);
        assert!(cache.should_evict(t2, timeout));
    }

    #[test]
    fn record_frame_updates_last_activity() {
        let start = Instant::now();
        let mut cache = StreamCache::new(sid(), header(), peer(), start);
        assert_eq!(cache.last_activity(), start);
        let t1 = start + Duration::from_millis(100);
        cache.record_frame(t1);
        assert_eq!(cache.last_activity(), t1);
    }
}
