//! APRS digipeater processing logic.
//!
//! Implements the three digipeater algorithms supported by the TH-D75
//! (per Operating Tips section 2.4):
//!
//! - **`UIdigipeat`**: Simple alias replacement. When a path entry matches
//!   a configured alias, replace it with our callsign and mark as used.
//! - **`UIflood`**: Decrement the hop count on a flooding alias (e.g., `CA3-3`).
//!   Drop when the count reaches zero.
//! - **`UItrace`**: Like `UIflood`, but also inserts our callsign into the
//!   path before the decremented hop entry.
//!
//! In addition, the [`DigipeaterConfig`] carries a rolling dedup cache so
//! that packets seen more than once within [`DigipeaterConfig::dedup_ttl`]
//! are not re-transmitted, and it performs own-callsign loop detection to
//! prevent relaying a packet that has already been through this station.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use super::{Ax25Address, Ax25Packet};

/// Default rolling dedup window for digipeater retransmission suppression.
///
/// A packet whose (source, destination, info) hash has been seen within
/// this interval will not be relayed a second time. 30 seconds is the
/// conventional value used by UIDIGI and other APRS digis.
pub const DEFAULT_DEDUP_TTL: Duration = Duration::from_secs(30);

/// Default viscous delay for fill-in digipeaters.
///
/// When nonzero, relay candidates are held for up to this duration to
/// let other digipeaters (with clearer paths) go first; if any digi
/// actually relays the packet within the window, we cancel our own
/// pending relay. Disabled (0) by default.
pub const DEFAULT_VISCOUS_DELAY: Duration = Duration::from_secs(0);

/// A typed digipeater alias.
///
/// APRS digipeater configurations use named aliases (`WIDE1`, `CA`,
/// `TRACE`, etc.) to identify which path entries should be relayed.
/// This newtype wraps the alias string with ergonomic equality checks
/// and validation (ASCII, uppercase, non-empty).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DigipeaterAlias(String);

impl DigipeaterAlias {
    /// Create a new alias, rejecting empty or non-ASCII input.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError::AprsWireOutOfRange`] on
    /// invalid input.
    pub fn new(s: &str) -> Result<Self, crate::error::ValidationError> {
        if s.is_empty() || !s.is_ascii() {
            return Err(crate::error::ValidationError::AprsWireOutOfRange {
                field: "DigipeaterAlias",
                detail: "must be non-empty ASCII",
            });
        }
        Ok(Self(s.to_ascii_uppercase()))
    }

    /// Return the alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DigipeaterAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Digipeater configuration.
///
/// Controls which packets are relayed and how the digipeater path is modified.
/// Also carries the rolling dedup cache used to suppress retransmission of
/// packets seen more than once within [`DigipeaterConfig::dedup_ttl`].
#[derive(Debug, Clone)]
pub struct DigipeaterConfig {
    /// Our callsign (used for `UIdigipeat` and `UItrace` path insertion).
    pub callsign: Ax25Address,
    /// `UIdigipeat` aliases (e.g., `["WIDE1-1"]`). Relay if path contains
    /// this alias, replace with our callsign + completion flag.
    pub uidigipeat_aliases: Vec<String>,
    /// `UIflood` alias base (e.g., `"CA"`). Relay and decrement hop count.
    /// The SSID encodes the remaining hop count.
    pub uiflood_alias: Option<String>,
    /// `UItrace` alias base (e.g., `"WIDE"`). Relay, decrement hop count,
    /// and insert our callsign in the path.
    pub uitrace_alias: Option<String>,
    /// How long a recently-seen packet is remembered in the dedup cache.
    /// Defaults to [`DEFAULT_DEDUP_TTL`] (30 s).
    pub dedup_ttl: Duration,
    /// Viscous delay — how long to hold a relay candidate before
    /// actually transmitting it. `0` disables the feature (default).
    ///
    /// Viscous digis defer relay for a short window so that nearby
    /// full digipeaters have a chance to transmit first; if any other
    /// digi relays the packet within the window, the viscous digi
    /// cancels its own pending relay. This lets a fill-in digi stay
    /// quiet in well-covered areas while still providing coverage in
    /// RF gaps.
    pub viscous_delay: Duration,
    /// Rolling cache of recently-relayed packet hashes. Populated on
    /// successful relay and pruned of expired entries on each call to
    /// [`Self::process`].
    dedup_cache: HashMap<u64, Instant>,
    /// Pending viscous relays, keyed on the packet hash. Each entry is
    /// the time we first saw the packet; when the delay elapses and
    /// we haven't seen anyone else relay it, we transmit ourselves.
    pending_viscous: HashMap<u64, (Instant, Ax25Packet)>,
}

impl DigipeaterConfig {
    /// Build a new config with an empty dedup cache and the default TTL.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(
        callsign: Ax25Address,
        uidigipeat_aliases: Vec<String>,
        uiflood_alias: Option<String>,
        uitrace_alias: Option<String>,
    ) -> Self {
        Self {
            callsign,
            uidigipeat_aliases,
            uiflood_alias,
            uitrace_alias,
            dedup_ttl: DEFAULT_DEDUP_TTL,
            viscous_delay: DEFAULT_VISCOUS_DELAY,
            dedup_cache: HashMap::new(),
            pending_viscous: HashMap::new(),
        }
    }

    /// Drain any pending viscous relays whose delay window has elapsed.
    ///
    /// Call this periodically (e.g. from the client event loop) to pick
    /// up relays whose viscous delay has expired without anyone else
    /// transmitting the same packet. Returns the frames ready to send.
    pub fn drain_ready_viscous(&mut self) -> Vec<Ax25Packet> {
        let now = Instant::now();
        let delay = self.viscous_delay;
        let mut ready = Vec::new();
        let mut remaining = HashMap::new();
        for (k, (t, p)) in self.pending_viscous.drain() {
            if now.duration_since(t) >= delay {
                ready.push(p);
                // Record this relay in the dedup cache to prevent
                // re-relaying if the packet comes around again.
                let _prev = self.dedup_cache.insert(k, now);
            } else {
                let _prev = remaining.insert(k, (t, p));
            }
        }
        self.pending_viscous = remaining;
        ready
    }
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Result of digipeater processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigiAction {
    /// Do not relay this packet (no alias matched).
    Drop,
    /// The packet was not a UI frame (control != 0x03 or PID != 0xF0).
    /// APRS uses only UI frames, so this is effectively a pass-through.
    NotUiFrame,
    /// Loop detected — our own callsign is already in the used path.
    LoopDetected,
    /// Duplicate packet — we already relayed this one within the TTL
    /// window.
    Duplicate,
    /// Relay with modified digipeater path.
    Relay {
        /// The packet with its path modified for retransmission.
        modified_packet: Ax25Packet,
    },
}

// ---------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------

impl DigipeaterConfig {
    /// Process an incoming AX.25 UI frame through digipeater logic.
    ///
    /// Performs, in order:
    /// 1. UI frame sanity (`control=0x03`, `PID=0xF0`).
    /// 2. Own-callsign loop detection — if our callsign appears anywhere
    ///    in the digipeater path with the H-bit set, the packet has already
    ///    been through us and we must drop it to prevent routing loops.
    /// 3. Dedup cache lookup — if we've relayed a packet with the same
    ///    source/destination/info hash within [`Self::dedup_ttl`], drop.
    /// 4. First-unused entry alias matching (`UIdigipeat`, `UIflood`,
    ///    `UItrace`).
    /// 5. On successful relay, the packet hash is recorded in the dedup
    ///    cache with the current time.
    ///
    /// Returns [`DigiAction::Drop`] if any check fails or no alias matches.
    pub fn process(&mut self, packet: &Ax25Packet) -> DigiAction {
        // --- 1. UI frame check ---
        if packet.control != 0x03 || packet.protocol != 0xF0 {
            return DigiAction::NotUiFrame;
        }

        // --- 2. Own-callsign loop detection ---
        if own_callsign_already_relayed(&self.callsign, &packet.digipeaters) {
            return DigiAction::LoopDetected;
        }

        // --- 3. Prune expired dedup entries and check ---
        self.prune_dedup();
        let packet_hash = hash_packet_identity(packet);
        if self.dedup_cache.contains_key(&packet_hash) {
            return DigiAction::Duplicate;
        }

        // --- 3a. Viscous cancellation ---
        // If we have a pending viscous relay for this packet and the
        // packet arrives again, it means someone else relayed it. Drop
        // the pending entry and suppress our own relay.
        if self.viscous_delay > Duration::from_secs(0)
            && self.pending_viscous.remove(&packet_hash).is_some()
        {
            let _prev = self.dedup_cache.insert(packet_hash, Instant::now());
            return DigiAction::Duplicate;
        }

        // --- 4. First-unused entry alias matching ---
        let Some(first_unused) = packet.digipeaters.iter().position(|d| !is_used_digi(d)) else {
            return DigiAction::Drop;
        };

        let digi = &packet.digipeaters[first_unused];

        let action = {
            let digi_str = format!("{digi}");
            if self
                .uidigipeat_aliases
                .iter()
                .any(|a| digi_str.eq_ignore_ascii_case(a))
            {
                apply_uidigipeat(&self.callsign, packet, first_unused)
            } else if self.uiflood_alias.as_deref().is_some_and(|a| {
                digi.callsign.as_str().eq_ignore_ascii_case(a) && digi.ssid.get() > 0
            }) {
                apply_uiflood(packet, first_unused)
            } else if self.uitrace_alias.as_deref().is_some_and(|a| {
                digi.callsign.as_str().eq_ignore_ascii_case(a) && digi.ssid.get() > 0
            }) {
                apply_uitrace(&self.callsign, packet, first_unused)
            } else {
                DigiAction::Drop
            }
        };

        // --- 5. Record successful relay in dedup cache ---
        if let DigiAction::Relay {
            ref modified_packet,
        } = action
        {
            if self.viscous_delay > Duration::from_secs(0) {
                // Defer the relay — hold it in the viscous queue. The
                // dedup cache is only populated once we actually
                // transmit (in `drain_ready_viscous`).
                let _prev = self
                    .pending_viscous
                    .insert(packet_hash, (Instant::now(), modified_packet.clone()));
                return DigiAction::Drop;
            }
            let _previous = self.dedup_cache.insert(packet_hash, Instant::now());
        }

        action
    }

    /// Remove dedup entries older than [`Self::dedup_ttl`].
    fn prune_dedup(&mut self) {
        let ttl = self.dedup_ttl;
        let now = Instant::now();
        self.dedup_cache.retain(|_, t| now.duration_since(*t) < ttl);
    }

    /// Number of entries currently in the dedup cache (for tests/metrics).
    #[must_use]
    pub fn dedup_cache_len(&self) -> usize {
        self.dedup_cache.len()
    }
}

/// Hash a packet's identity tuple `(source, destination, info)` for dedup.
///
/// Uses `DefaultHasher` which is SipHash-1-3 in std. The hash is only used
/// locally within one process lifetime for dedup, so randomized seeding is
/// fine (actually preferred, as it makes the cache unpredictable).
fn hash_packet_identity(packet: &Ax25Packet) -> u64 {
    let mut h = DefaultHasher::new();
    packet.source.callsign.as_str().hash(&mut h);
    packet.source.ssid.get().hash(&mut h);
    packet.destination.callsign.as_str().hash(&mut h);
    packet.destination.ssid.get().hash(&mut h);
    packet.info.hash(&mut h);
    h.finish()
}

/// Check whether our callsign appears in the digipeater path with the
/// has-been-repeated bit set. If so, the packet has already passed through
/// this station and relaying it again would create a routing loop.
fn own_callsign_already_relayed(own: &Ax25Address, path: &[Ax25Address]) -> bool {
    path.iter().any(|d| {
        d.repeated
            && d.callsign
                .as_str()
                .eq_ignore_ascii_case(own.callsign.as_str())
            && d.ssid == own.ssid
    })
}

/// `UIdigipeat`: replace the alias entry with our callsign, marked as used.
fn apply_uidigipeat(callsign: &Ax25Address, packet: &Ax25Packet, idx: usize) -> DigiAction {
    let mut modified = packet.clone();
    modified.digipeaters[idx] = mark_used(callsign);
    DigiAction::Relay {
        modified_packet: modified,
    }
}

/// `UIflood`: decrement the hop count. Mark as used when exhausted.
fn apply_uiflood(packet: &Ax25Packet, idx: usize) -> DigiAction {
    let digi = &packet.digipeaters[idx];
    let new_ssid_raw = digi.ssid.get().saturating_sub(1);
    // SSID is already validated 0-15, and new_ssid_raw is strictly
    // smaller, so `new(...)` cannot fail.
    let new_ssid =
        crate::types::aprs_wire::Ssid::new(new_ssid_raw).expect("decremented SSID stays in 0..=15");

    let mut modified = packet.clone();
    if new_ssid_raw == 0 {
        modified.digipeaters[idx] = mark_used(&Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: crate::types::aprs_wire::Ssid::ZERO,
            repeated: false,
            c_bit: false,
        });
    } else {
        modified.digipeaters[idx] = Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: new_ssid,
            repeated: false,
            c_bit: false,
        };
    }
    DigiAction::Relay {
        modified_packet: modified,
    }
}

/// `UItrace`: like `UIflood` but also inserts our callsign before the hop entry.
fn apply_uitrace(callsign: &Ax25Address, packet: &Ax25Packet, idx: usize) -> DigiAction {
    // AX.25 supports at most 8 digipeater entries.
    if packet.digipeaters.len() >= 8 {
        return DigiAction::Drop;
    }

    let digi = &packet.digipeaters[idx];
    let new_ssid_raw = digi.ssid.get().saturating_sub(1);
    let new_ssid =
        crate::types::aprs_wire::Ssid::new(new_ssid_raw).expect("decremented SSID stays in 0..=15");

    let mut modified = packet.clone();

    // Insert our callsign (marked as used) before the current entry.
    modified.digipeaters.insert(idx, mark_used(callsign));

    // The original entry shifted to idx+1; update its hop count.
    let trace_idx = idx + 1;
    if new_ssid_raw == 0 {
        modified.digipeaters[trace_idx] = mark_used(&Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: crate::types::aprs_wire::Ssid::ZERO,
            repeated: false,
            c_bit: false,
        });
    } else {
        modified.digipeaters[trace_idx] = Ax25Address {
            callsign: digi.callsign.clone(),
            ssid: new_ssid,
            repeated: false,
            c_bit: false,
        };
    }

    DigiAction::Relay {
        modified_packet: modified,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if a digipeater entry has been used (has-been-repeated).
const fn is_used_digi(addr: &Ax25Address) -> bool {
    addr.repeated
}

/// Create a copy of an address with the H-bit (has-been-repeated) set.
fn mark_used(addr: &Ax25Address) -> Ax25Address {
    Ax25Address {
        callsign: addr.callsign.clone(),
        ssid: addr.ssid,
        repeated: true,
        c_bit: addr.c_bit,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_addr(call: &str, ssid: u8) -> Ax25Address {
        // If call ends with '*', strip it and set repeated=true.
        let (callsign, repeated) = call
            .strip_suffix('*')
            .map_or_else(|| (call.to_owned(), false), |s| (s.to_owned(), true));
        let mut addr = Ax25Address::new(&callsign, ssid);
        addr.repeated = repeated;
        addr
    }

    fn make_packet(digipeaters: Vec<Ax25Address>) -> Ax25Packet {
        Ax25Packet {
            source: make_addr("N0CALL", 7),
            destination: make_addr("APK005", 0),
            digipeaters,
            control: 0x03,
            protocol: 0xF0,
            info: b"!3518.00N/08414.00W-test".to_vec(),
        }
    }

    fn make_config() -> DigipeaterConfig {
        DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec!["WIDE1-1".to_owned()],
            Some("CA".to_owned()),
            Some("WIDE".to_owned()),
        )
    }

    // ---- UIdigipeat tests ----

    #[test]
    fn uidigipeat_matches_alias() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("WIDE1", 1), make_addr("WIDE2", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters[0].callsign, "MYDIGI");
                assert!(modified_packet.digipeaters[0].repeated);
                assert_eq!(modified_packet.digipeaters[0].ssid, 0);
                // Second entry unchanged.
                assert_eq!(modified_packet.digipeaters[1].callsign, "WIDE2");
                assert_eq!(modified_packet.digipeaters[1].ssid, 1);
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn uidigipeat_skips_used_entries() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("N1ABC*", 0), make_addr("WIDE1", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                // First entry untouched (already used).
                assert_eq!(modified_packet.digipeaters[0].callsign, "N1ABC");
                assert!(modified_packet.digipeaters[0].repeated);
                // Second entry replaced.
                assert_eq!(modified_packet.digipeaters[1].callsign, "MYDIGI");
                assert!(modified_packet.digipeaters[1].repeated);
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn uidigipeat_no_match_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("RELAY", 0)]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    #[test]
    fn uidigipeat_all_used_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("WIDE1*", 1)]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    // ---- UIflood tests ----

    #[test]
    fn uiflood_decrements_hop() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("N1ABC*", 0), make_addr("CA", 3)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters[1].callsign, "CA");
                assert_eq!(modified_packet.digipeaters[1].ssid, 2);
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn uiflood_last_hop_marks_used() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("CA", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters[0].callsign, "CA");
                assert!(modified_packet.digipeaters[0].repeated);
                assert_eq!(modified_packet.digipeaters[0].ssid, 0);
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn uiflood_zero_ssid_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("CA", 0)]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    // ---- UItrace tests ----

    #[test]
    fn uitrace_inserts_callsign_and_decrements() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("WIDE", 3)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                // Our callsign inserted first, marked used.
                assert_eq!(modified_packet.digipeaters[0].callsign, "MYDIGI");
                assert!(modified_packet.digipeaters[0].repeated);
                assert_eq!(modified_packet.digipeaters[0].ssid, 0);
                // Original entry with decremented hop.
                assert_eq!(modified_packet.digipeaters[1].callsign, "WIDE");
                assert_eq!(modified_packet.digipeaters[1].ssid, 2);
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn uitrace_last_hop_marks_exhausted() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("WIDE", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                assert_eq!(modified_packet.digipeaters[0].callsign, "MYDIGI");
                assert!(modified_packet.digipeaters[0].repeated);
                assert_eq!(modified_packet.digipeaters[1].callsign, "WIDE");
                assert!(modified_packet.digipeaters[1].repeated);
                assert_eq!(modified_packet.digipeaters[1].ssid, 0);
            }
            other => panic!("expected Relay, got {other:?}"),
        }
    }

    #[test]
    fn uitrace_full_path_drops() {
        let mut config = make_config();
        // 8 digipeaters = maximum, can't insert another.
        let mut digis: Vec<Ax25Address> = (0..8).map(|i| make_addr("USED*", i)).collect();
        // Replace last one with an unused WIDE entry.
        digis[7] = make_addr("WIDE", 2);

        // But the first unused is at index 7, and there are already 8 entries.
        let packet = make_packet(digis);
        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    // ---- Edge cases ----

    #[test]
    fn non_ui_frame_yields_not_ui_frame() {
        let mut config = make_config();
        let mut packet = make_packet(vec![make_addr("WIDE1", 1)]);
        packet.control = 0x01; // Not a UI frame.

        assert_eq!(config.process(&packet), DigiAction::NotUiFrame);
    }

    #[test]
    fn empty_digipeater_path_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![]);

        assert_eq!(config.process(&packet), DigiAction::Drop);
    }

    #[test]
    fn case_insensitive_alias_match() {
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec!["wide1-1".to_owned()],
            None,
            None,
        );
        let packet = make_packet(vec![make_addr("WIDE1", 1)]);

        match config.process(&packet) {
            DigiAction::Relay { .. } => {}
            other => panic!("expected case-insensitive match, got {other:?}"),
        }
    }

    #[test]
    fn uitrace_priority_over_flood_when_both_configured() {
        // If both uiflood and uitrace are configured for different aliases,
        // the correct one should match.
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![],
            Some("CA".to_owned()),
            Some("WIDE".to_owned()),
        );

        // UIflood packet (distinct info so dedup doesn't fire between cases).
        let mut flood_pkt = make_packet(vec![make_addr("CA", 2)]);
        flood_pkt.info = b"!3518.00N/08414.00W-flood".to_vec();
        match config.process(&flood_pkt) {
            DigiAction::Relay { modified_packet } => {
                // Should NOT insert callsign (flood, not trace).
                assert_eq!(modified_packet.digipeaters.len(), 1);
                assert_eq!(modified_packet.digipeaters[0].ssid, 1);
            }
            other => panic!("expected flood relay, got {other:?}"),
        }

        // UItrace packet.
        let mut trace_pkt = make_packet(vec![make_addr("WIDE", 2)]);
        trace_pkt.info = b"!3518.00N/08414.00W-trace".to_vec();
        match config.process(&trace_pkt) {
            DigiAction::Relay { modified_packet } => {
                // Should insert callsign (trace).
                assert_eq!(modified_packet.digipeaters.len(), 2);
            }
            other => panic!("expected trace relay, got {other:?}"),
        }
    }

    // ---- Dedup cache tests ----

    #[test]
    fn duplicate_packet_within_window_is_dropped() {
        let mut config = make_config();
        let packet = make_packet(vec![make_addr("WIDE1", 1)]);

        // First sighting → relay.
        assert!(matches!(config.process(&packet), DigiAction::Relay { .. }));
        assert_eq!(config.dedup_cache_len(), 1);

        // Second sighting within TTL → duplicate.
        let packet_2 = make_packet(vec![make_addr("WIDE1", 1)]);
        assert_eq!(config.process(&packet_2), DigiAction::Duplicate);
    }

    #[test]
    fn dedup_distinguishes_different_info() {
        let mut config = make_config();
        let mut p1 = make_packet(vec![make_addr("WIDE1", 1)]);
        let mut p2 = make_packet(vec![make_addr("WIDE1", 1)]);
        p1.info = b"!3518.00N/08414.00W-one".to_vec();
        p2.info = b"!3518.00N/08414.00W-two".to_vec();

        assert!(matches!(config.process(&p1), DigiAction::Relay { .. }));
        // Different info → different hash → should relay.
        assert!(matches!(config.process(&p2), DigiAction::Relay { .. }));
    }

    #[test]
    fn dedup_prunes_expired_entries() {
        let mut config = make_config();
        // Zero TTL so any "past" entry is instantly expired.
        config.dedup_ttl = Duration::from_secs(0);

        let packet = make_packet(vec![make_addr("WIDE1", 1)]);
        assert!(matches!(config.process(&packet), DigiAction::Relay { .. }));
        // With zero TTL the previous entry is pruned, so the same packet
        // can be relayed again.
        assert!(matches!(config.process(&packet), DigiAction::Relay { .. }));
    }

    #[test]
    fn viscous_delay_defers_initial_relay() {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(5);
        let packet = make_packet(vec![make_addr("WIDE1", 1)]);
        // With viscous_delay enabled, the first sighting is deferred.
        assert_eq!(config.process(&packet), DigiAction::Drop);
        assert_eq!(config.drain_ready_viscous().len(), 0);
    }

    #[test]
    fn viscous_delay_cancels_if_someone_else_relays() {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(5);
        let packet = make_packet(vec![make_addr("WIDE1", 1)]);
        // Defer.
        assert_eq!(config.process(&packet), DigiAction::Drop);
        // Same packet arrives again (someone else relayed).
        assert_eq!(config.process(&packet), DigiAction::Duplicate);
        // Drained queue is empty because the pending relay was cancelled.
        assert_eq!(config.drain_ready_viscous().len(), 0);
    }

    #[test]
    fn viscous_delay_zero_fires_immediately() {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(0);
        let packet = make_packet(vec![make_addr("WIDE1", 1)]);
        assert!(matches!(config.process(&packet), DigiAction::Relay { .. }));
    }

    #[test]
    fn own_callsign_with_h_bit_set_is_loop_detected() {
        let mut config = make_config(); // our callsign is MYDIGI
        // Packet already shows us as a used digi — must not be re-relayed.
        let packet = make_packet(vec![make_addr("MYDIGI*", 0), make_addr("WIDE2", 1)]);
        assert_eq!(config.process(&packet), DigiAction::LoopDetected);
    }

    #[test]
    fn own_callsign_unused_still_processes_first_entry() {
        let mut config = make_config();
        // Our callsign appears later in the path but the first entry is an
        // alias we should handle. The loop detector only trips on H-bit set.
        let packet = make_packet(vec![make_addr("WIDE1", 1), make_addr("MYDIGI", 0)]);
        assert!(matches!(config.process(&packet), DigiAction::Relay { .. }));
    }
}
