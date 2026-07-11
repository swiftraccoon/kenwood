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
//!
//! # Time handling
//!
//! Per the crate-level convention, this module is sans-io and never calls
//! `std::time::Instant::now()` internally. Every stateful method accepts
//! a `now: Instant` parameter; callers (typically the tokio shell) read
//! the wall clock once per iteration and thread it down.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use ax25_codec::{Ax25Address, Ax25Packet, MAX_DIGIPEATERS, RouteEntry, Ssid};

#[cfg(test)]
use ax25_codec::CommandResponse;

use crate::error::AprsError;

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
    /// Returns [`AprsError::InvalidDigipeaterAlias`] on invalid input.
    pub fn new(s: &str) -> Result<Self, AprsError> {
        if s.is_empty() || !s.is_ascii() {
            return Err(AprsError::InvalidDigipeaterAlias("must be non-empty ASCII"));
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
    /// Rolling cache of recently-relayed packet hashes, mapping each hash to
    /// the time it was last relayed. Populated on successful relay. Duplicate
    /// detection reads the stored timestamp directly (an entry counts as a
    /// duplicate only while it is younger than [`Self::dedup_ttl`]), so the
    /// expired-entry sweep ([`Self::prune_dedup`]) is purely memory
    /// reclamation and runs amortized rather than on every [`Self::process`].
    dedup_cache: HashMap<u64, Instant>,
    /// When the dedup cache was last swept of expired entries. `None` until
    /// the first sweep. Used to throttle [`Self::prune_dedup`] to at most one
    /// full pass per [`Self::dedup_ttl`]; correctness does not depend on it
    /// because the duplicate check is timestamp-aware.
    last_prune: Option<Instant>,
    /// Pending viscous relays, keyed on the packet hash. Each entry is
    /// the time we first saw the packet; when the delay elapses and
    /// we haven't seen anyone else relay it, we transmit ourselves.
    pending_viscous: HashMap<u64, (Instant, Ax25Packet)>,
}

impl DigipeaterConfig {
    /// Build a new config with an empty dedup cache and the default TTL.
    #[must_use]
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
            last_prune: None,
            pending_viscous: HashMap::new(),
        }
    }

    /// Drain any pending viscous relays whose delay window has elapsed.
    ///
    /// Call this periodically (e.g. from the client event loop) to pick
    /// up relays whose viscous delay has expired without anyone else
    /// transmitting the same packet. Returns the frames ready to send.
    ///
    /// The caller provides `now` so this module remains sans-io; pass the
    /// same `Instant` used for the surrounding [`Self::process`] calls.
    pub fn drain_ready_viscous(&mut self, now: Instant) -> Vec<Ax25Packet> {
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
    /// The packet was not a UI frame (the control byte does not
    /// classify as Unnumbered Information — P/F bit permitted, so both
    /// `0x03` and `0x13` are UI — or PID != 0xF0). APRS uses only UI
    /// frames, so this is effectively a pass-through.
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
    /// 1. UI frame sanity: the control byte must classify as
    ///    Unnumbered Information via [`Ax25Packet::is_ui`] (which
    ///    accepts the P/F bit, so `0x13` counts as UI alongside the
    ///    plain `0x03`) and the PID must be `0xF0`.
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
    /// The caller provides `now` so this module remains sans-io. Passing
    /// the same `Instant` to every stateful call in a single loop
    /// iteration keeps timing invariants consistent.
    ///
    /// Returns [`DigiAction::Drop`] if any check fails or no alias matches.
    pub fn process(&mut self, packet: &Ax25Packet, now: Instant) -> DigiAction {
        // --- 1. UI frame check ---
        // Typed classification instead of a raw `!= 0x03` compare: UI
        // frames may legally carry the P/F bit (control 0x13), and
        // `is_ui()` masks it off before matching.
        if !packet.is_ui() || packet.protocol != 0xF0 {
            return DigiAction::NotUiFrame;
        }

        // --- 2. Own-callsign loop detection ---
        if own_callsign_already_relayed(&self.callsign, &packet.digipeaters) {
            return DigiAction::LoopDetected;
        }

        // --- 3. Dedup check (timestamp-aware), with amortized prune ---
        // The duplicate decision reads the stored relay time directly so it
        // stays correct regardless of when the cache is next swept; the sweep
        // below is throttled to at most once per `dedup_ttl` purely to bound
        // memory. An entry counts as a duplicate only while younger than the
        // TTL — a stale entry that has not yet been reclaimed is ignored.
        self.prune_dedup(now);
        let packet_hash = hash_packet_identity(packet);
        if self
            .dedup_cache
            .get(&packet_hash)
            .is_some_and(|&t| now.duration_since(t) < self.dedup_ttl)
        {
            return DigiAction::Duplicate;
        }

        // --- 3a. Viscous cancellation ---
        // If we have a pending viscous relay for this packet and the
        // packet arrives again, it means someone else relayed it. Drop
        // the pending entry and suppress our own relay.
        if self.viscous_delay > Duration::from_secs(0)
            && self.pending_viscous.remove(&packet_hash).is_some()
        {
            let _prev = self.dedup_cache.insert(packet_hash, now);
            return DigiAction::Duplicate;
        }

        // --- 4. First-unused entry alias matching ---
        let Some(first_unused) = packet.digipeaters.iter().position(|d| !is_used_digi(d)) else {
            return DigiAction::Drop;
        };

        let Some(digi) = packet.digipeaters.get(first_unused) else {
            // `position` just returned `Some(first_unused)`, so this
            // branch is unreachable; fall through as a drop to preserve
            // the "no relay" invariant without panicking.
            return DigiAction::Drop;
        };

        // Match the first unused entry against each alias family in turn.
        // `UIflood`/`UItrace` use New-N-Paradigm matching (callsign is the
        // base plus a baked-in `n` digit, with N remaining hops in the SSID);
        // see `matches_new_n_alias` for the on-wire encoding. `UIdigipeat`
        // matches a verbatim `CALL`/`CALL-SSID` alias token. None of these
        // allocate per packet.
        let callsign = digi.address.callsign.as_str();
        let hops_remaining = digi.address.ssid.get() > 0;
        let action = if self
            .uidigipeat_aliases
            .iter()
            .any(|a| uidigipeat_alias_matches(&digi.address, a))
        {
            apply_uidigipeat(&self.callsign, packet, first_unused)
        } else if self
            .uiflood_alias
            .as_deref()
            .is_some_and(|a| hops_remaining && matches_new_n_alias(callsign, a))
        {
            apply_uiflood(packet, first_unused)
        } else if self
            .uitrace_alias
            .as_deref()
            .is_some_and(|a| hops_remaining && matches_new_n_alias(callsign, a))
        {
            apply_uitrace(&self.callsign, packet, first_unused)
        } else {
            DigiAction::Drop
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
                    .insert(packet_hash, (now, modified_packet.clone()));
                return DigiAction::Drop;
            }
            let _previous = self.dedup_cache.insert(packet_hash, now);
        }

        action
    }

    /// Remove dedup entries older than [`Self::dedup_ttl`], at most once per
    /// TTL window.
    ///
    /// This is a memory-reclamation pass only — duplicate detection in
    /// [`Self::process`] is timestamp-aware and does not depend on expired
    /// entries having already been swept. Throttling the full `retain` sweep
    /// to one pass per [`Self::dedup_ttl`] keeps `process` amortized O(1) in
    /// the common case instead of O(cache size) on every call. An entry can
    /// therefore outlive its TTL by up to one extra `dedup_ttl` before being
    /// reclaimed, which is harmless: it is already treated as non-duplicate.
    fn prune_dedup(&mut self, now: Instant) {
        let ttl = self.dedup_ttl;
        // Sweep when we have never swept, when the throttle window has fully
        // elapsed, or when the clock appears to have gone backwards (a
        // monotonic-`Instant` caller should never do this, but if `now`
        // precedes the recorded prune time, fall back to sweeping).
        let due = self
            .last_prune
            .is_none_or(|last| now.checked_duration_since(last).is_none_or(|d| d >= ttl));
        if !due {
            return;
        }
        self.dedup_cache.retain(|_, t| now.duration_since(*t) < ttl);
        self.last_prune = Some(now);
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
fn own_callsign_already_relayed(own: &Ax25Address, path: &[RouteEntry]) -> bool {
    path.iter().any(|d| {
        d.has_repeated
            && d.address
                .callsign
                .as_str()
                .eq_ignore_ascii_case(own.callsign.as_str())
            && d.address.ssid == own.ssid
    })
}

/// `UIdigipeat`: replace the alias entry with our callsign, marked as used.
fn apply_uidigipeat(callsign: &Ax25Address, packet: &Ax25Packet, idx: usize) -> DigiAction {
    let mut modified = packet.clone();
    if let Some(slot) = modified.digipeaters.get_mut(idx) {
        *slot = RouteEntry {
            address: callsign.clone(),
            has_repeated: true,
        };
    } else {
        // Caller only invokes this with an `idx` produced by `position`
        // on `packet.digipeaters`, so the slot is always present. If
        // the packet has been mutated in the meantime, prefer a drop
        // over a panic.
        return DigiAction::Drop;
    }
    DigiAction::Relay {
        modified_packet: modified,
    }
}

/// `UIflood`: decrement the hop count. Mark as used when exhausted.
fn apply_uiflood(packet: &Ax25Packet, idx: usize) -> DigiAction {
    let Some(digi) = packet.digipeaters.get(idx) else {
        return DigiAction::Drop;
    };
    let new_ssid_raw = digi.address.ssid.get().saturating_sub(1);
    // SSID is already validated 0-15, and new_ssid_raw is strictly
    // smaller, so `new(...)` cannot fail. Fall back to zero if the
    // codec's validator ever disagrees.
    let new_ssid = Ssid::new(new_ssid_raw).unwrap_or(Ssid::ZERO);
    let callsign = digi.address.callsign.clone();

    let mut modified = packet.clone();
    let Some(slot) = modified.digipeaters.get_mut(idx) else {
        return DigiAction::Drop;
    };
    if new_ssid_raw == 0 {
        *slot = RouteEntry {
            address: Ax25Address::from_parts(callsign, Ssid::ZERO),
            has_repeated: true,
        };
    } else {
        *slot = RouteEntry {
            address: Ax25Address::from_parts(callsign, new_ssid),
            has_repeated: false,
        };
    }
    DigiAction::Relay {
        modified_packet: modified,
    }
}

/// `UItrace`: like `UIflood` but also inserts our callsign before the hop entry.
fn apply_uitrace(callsign: &Ax25Address, packet: &Ax25Packet, idx: usize) -> DigiAction {
    // `UItrace` inserts a new digipeater slot; if the path is already at
    // the codec-level maximum (see `ax25_codec::MAX_DIGIPEATERS`, currently
    // 8 per AX.25 v2.0 / Linux `AX25_MAX_DIGIS` convention), we must drop
    // rather than overflow the slot count.
    if packet.digipeaters.len() >= MAX_DIGIPEATERS {
        return DigiAction::Drop;
    }

    // Snapshot the alias digipeater's callsign + current hop count;
    // after `modified.digipeaters.insert` the indices shift and we can
    // no longer borrow from the original slice without re-indexing.
    let Some(source_digi) = packet.digipeaters.get(idx) else {
        return DigiAction::Drop;
    };
    let alias_callsign = source_digi.address.callsign.clone();
    let new_ssid_raw = source_digi.address.ssid.get().saturating_sub(1);
    let new_ssid = Ssid::new(new_ssid_raw).unwrap_or(Ssid::ZERO);

    let mut modified = packet.clone();

    // Insert our callsign (marked as used) before the current entry.
    modified.digipeaters.insert(
        idx,
        RouteEntry {
            address: callsign.clone(),
            has_repeated: true,
        },
    );

    // The original entry shifted to idx+1; update its hop count.
    let trace_idx = idx + 1;
    let Some(slot) = modified.digipeaters.get_mut(trace_idx) else {
        return DigiAction::Drop;
    };
    if new_ssid_raw == 0 {
        *slot = RouteEntry {
            address: Ax25Address::from_parts(alias_callsign, Ssid::ZERO),
            has_repeated: true,
        };
    } else {
        *slot = RouteEntry {
            address: Ax25Address::from_parts(alias_callsign, new_ssid),
            has_repeated: false,
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
const fn is_used_digi(entry: &RouteEntry) -> bool {
    entry.has_repeated
}

/// Match a path-entry callsign against a New-N-Paradigm flood/trace alias base.
///
/// The New-N Paradigm (`WIDEn-N`, `TRACEn-N`, `SSn-N`) encodes the alias on
/// the wire as `<base><n>-<N>`, where `<base>` is the configured alias (e.g.
/// `WIDE`, `CA`), `n` is the *originally requested* hop count baked into the
/// callsign field, and `N` is the *remaining* hop count carried in the SSID.
/// Because the trailing `n` digit is part of the AX.25 callsign field, an
/// on-wire `WIDE2-2` entry decodes (see `ax25_codec::parse_ax25`) to
/// `callsign == "WIDE2"`, `ssid == 2` — **not** `callsign == "WIDE"`.
///
/// This returns `true` when `callsign` is either:
/// - the New-N form `<base>` followed by exactly one decimal digit `1..=7`
///   (e.g. base `WIDE` matches `WIDE1`..`WIDE7`), or
/// - the literal-base form where the callsign equals `<base>` exactly (e.g.
///   base `CA` matches a `CA-3` wire entry whose callsign field is `CA`); the
///   remaining hop count then lives entirely in the SSID.
///
/// Matching is ASCII-case-insensitive on the base. The caller is responsible
/// for the `ssid > 0` (hops-remaining) gate and the has-been-repeated gate.
fn matches_new_n_alias(callsign: &str, base: &str) -> bool {
    // Literal-base form: callsign is exactly the base (e.g. "CA" / "WIDE"),
    // with the hop count carried solely in the SSID.
    if callsign.eq_ignore_ascii_case(base) {
        return true;
    }
    // New-N form: callsign is `<base>` + one decimal digit n in 1..=7.
    let Some(rest) = strip_prefix_ignore_ascii_case(callsign, base) else {
        return false;
    };
    let [digit] = rest.as_bytes() else {
        return false;
    };
    matches!(digit, b'1'..=b'7')
}

/// Case-insensitive [`str::strip_prefix`]: if `s` begins with `prefix`
/// (comparing ASCII case-insensitively), return the remainder after the
/// prefix; otherwise `None`.
fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let split = prefix.len();
    let head = s.get(..split)?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(split..)
    } else {
        None
    }
}

/// Compare a digipeater address against a full `UIdigipeat` alias string
/// (e.g. `"WIDE1-1"`, `"RELAY"`) without allocating.
///
/// `UIdigipeat` aliases are matched as complete `CALL` or `CALL-SSID` tokens
/// (no New-N digit synthesis — the alias is taken verbatim). The comparison
/// is ASCII-case-insensitive and mirrors the `Ax25Address` `Display` form
/// (`CALL` when the SSID is zero, otherwise `CALL-SSID`) so it stays
/// behaviourally identical to the previous `format!`-based check.
fn uidigipeat_alias_matches(addr: &Ax25Address, alias: &str) -> bool {
    let (alias_call, alias_ssid) = alias
        .split_once('-')
        .map_or((alias, None), |(c, s)| (c, Some(s)));
    if !addr.callsign.as_str().eq_ignore_ascii_case(alias_call) {
        return false;
    }
    // Resolve the alias to a single expected SSID and compare:
    // - `WIDE1-1` form: the suffix must parse to the address SSID.
    // - bare `RELAY` form (no `-SSID`): `Display` omits the SSID only when it
    //   is zero, so the address must carry SSID 0 to match.
    let expected_ssid = match alias_ssid {
        Some(s) => match s.parse::<u8>() {
            Ok(n) => n,
            Err(_) => return false,
        },
        None => 0,
    };
    addr.ssid.get() == expected_ssid
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn make_addr(call: &str, ssid: u8) -> Ax25Address {
        let callsign = call.strip_suffix('*').unwrap_or(call);
        Ax25Address::new(callsign, ssid)
            .unwrap_or_else(|_| unreachable!("test fixture callsign is statically valid"))
    }

    fn make_digi(call: &str, ssid: u8) -> RouteEntry {
        // If call ends with '*', strip it and set has_repeated=true.
        let (callsign, has_repeated) = call
            .strip_suffix('*')
            .map_or_else(|| (call, false), |s| (s, true));
        let mut entry = RouteEntry::new(callsign, ssid)
            .unwrap_or_else(|_| unreachable!("test fixture callsign is statically valid"));
        entry.has_repeated = has_repeated;
        entry
    }

    fn make_packet(digipeaters: Vec<RouteEntry>) -> Ax25Packet {
        Ax25Packet {
            source: make_addr("N0CALL", 7),
            destination: make_addr("APK005", 0),
            digipeaters,
            command_or_response: Some(CommandResponse::Command),
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
    fn uidigipeat_matches_alias() -> TestResult {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE1", 1), make_digi("WIDE2", 1)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "MYDIGI");
                assert!(d0.has_repeated);
                assert_eq!(d0.address.ssid, 0);
                // Second entry unchanged.
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "WIDE2");
                assert_eq!(d1.address.ssid, 1);
            }
            other => return Err(format!("expected Relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uidigipeat_skips_used_entries() -> TestResult {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("N1ABC*", 0), make_digi("WIDE1", 1)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                // First entry untouched (already used).
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "N1ABC");
                assert!(d0.has_repeated);
                // Second entry replaced.
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "MYDIGI");
                assert!(d1.has_repeated);
            }
            other => return Err(format!("expected Relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uidigipeat_no_match_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("RELAY", 0)]);
        let t0 = Instant::now();

        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    #[test]
    fn uidigipeat_all_used_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE1*", 1)]);
        let t0 = Instant::now();

        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    // ---- UIflood tests ----

    #[test]
    fn uiflood_decrements_hop() -> TestResult {
        // Literal-base wire form `CA-3`: the callsign field is exactly the
        // alias base "CA" and the SSID carries the full hop count (3). This
        // is distinct from the New-N form (`CA7-3` → callsign "CA7"), which
        // is exercised by the `*_new_n_*` tests below.
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("N1ABC*", 0), make_digi("CA", 3)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "CA");
                assert_eq!(d1.address.ssid, 2);
            }
            other => return Err(format!("expected Relay, got {other:?}").into()),
        }
        Ok(())
    }

    /// Round-trip a packet through the real AX.25 codec so the digipeater
    /// path is built from genuine on-wire bytes, then assert the decoded
    /// path matches the expected `(callsign, ssid, has_repeated)` tuples.
    ///
    /// This proves the New-N wire encoding our matcher must handle: an
    /// on-wire `WIDE2-2` entry decodes to `callsign == "WIDE2"`, `ssid == 2`
    /// (the requested-hops digit is part of the callsign field; the
    /// remaining hops live in the SSID).
    fn parsed_packet(
        digipeaters: Vec<RouteEntry>,
        info: &[u8],
    ) -> Result<Ax25Packet, Box<dyn std::error::Error>> {
        let packet = Ax25Packet {
            source: make_addr("N0CALL", 7),
            destination: make_addr("APK005", 0),
            digipeaters,
            command_or_response: Some(CommandResponse::Command),
            control: 0x03,
            protocol: 0xF0,
            info: info.to_vec(),
        };
        let bytes = ax25_codec::build_ax25(&packet);
        let parsed = ax25_codec::parse_ax25(&bytes)?;
        assert_eq!(parsed, packet, "codec round-trip must be lossless");
        Ok(parsed)
    }

    #[test]
    fn new_n_wire_form_decodes_digit_into_callsign() -> TestResult {
        // Sanity-check the on-wire model our matcher relies on: `WIDE2-2`
        // round-trips through the codec to callsign "WIDE2" + ssid 2.
        let packet = parsed_packet(vec![make_digi("WIDE2", 2)], b"!new-n wire")?;
        let d0 = packet.digipeaters.first().ok_or("missing digi 0")?;
        assert_eq!(d0.address.callsign, "WIDE2");
        assert_eq!(d0.address.ssid, 2);
        assert!(!d0.has_repeated);
        Ok(())
    }

    #[test]
    fn uitrace_new_n_widen_n_is_relayed_not_dropped() -> TestResult {
        // The canonical New-N traceable path `WIDE2-2` (callsign "WIDE2",
        // ssid 2 on the wire) MUST be relayed by the UItrace alias base
        // "WIDE". Before the fix this fell through to Drop because the whole
        // callsign "WIDE2" was compared against the bare base "WIDE".
        let mut config = make_config(); // uitrace base = "WIDE"
        let packet = parsed_packet(vec![make_digi("WIDE2", 2)], b"!widen trace")?;
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                // UItrace inserts our callsign, then decrements the SSID
                // (WIDE2-2 -> WIDE2-1) while preserving the "WIDE2" callsign.
                assert_eq!(modified_packet.digipeaters.len(), 2);
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "MYDIGI");
                assert!(d0.has_repeated);
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "WIDE2");
                assert_eq!(d1.address.ssid, 1);
                assert!(!d1.has_repeated);
            }
            other => return Err(format!("expected New-N trace relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uitrace_new_n_last_hop_marks_used() -> TestResult {
        // `WIDE7-1` (callsign "WIDE7", ssid 1) is a final traceable hop:
        // decrement to ssid 0 and mark used. (We avoid `WIDE1-1` here, which
        // the default config also lists as a verbatim UIdigipeat alias and
        // would match with higher precedence — see the dedicated precedence
        // test.) Use a config whose only alias family is the "WIDE" trace
        // base so this exclusively exercises the New-N UItrace last hop.
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![],
            None,
            Some("WIDE".to_owned()),
        );
        let packet = parsed_packet(vec![make_digi("WIDE7", 1)], b"!widen last")?;
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "MYDIGI");
                assert!(d0.has_repeated);
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "WIDE7");
                assert_eq!(d1.address.ssid, 0);
                assert!(d1.has_repeated);
            }
            other => return Err(format!("expected New-N trace relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uiflood_new_n_ssn_n_is_relayed_not_dropped() -> TestResult {
        // Generic flooding alias `CA` matching the New-N form `CA7-3`
        // (callsign "CA7", ssid 3 on the wire). UIflood decrements the SSID
        // in place without inserting our callsign: CA7-3 -> CA7-2.
        let mut config = make_config(); // uiflood base = "CA"
        let packet = parsed_packet(vec![make_digi("CA7", 3)], b"!ssn flood")?;
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 1);
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "CA7");
                assert_eq!(d0.address.ssid, 2);
                assert!(!d0.has_repeated);
            }
            other => return Err(format!("expected New-N flood relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn new_n_first_unused_after_used_widen() -> TestResult {
        // A realistic two-hop path `WIDE1*,WIDE2-1`: the first hop is already
        // used (H-bit set), so the digipeater must act on the second entry
        // `WIDE2-1` (callsign "WIDE2", ssid 1) via the "WIDE" trace base.
        let mut config = make_config();
        let packet = parsed_packet(
            vec![make_digi("WIDE1*", 0), make_digi("WIDE2", 1)],
            b"!two hop",
        )?;
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                // Our callsign inserted before the (now-used) WIDE2 entry.
                assert_eq!(modified_packet.digipeaters.len(), 3);
                let used0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(used0.address.callsign, "WIDE1");
                assert!(used0.has_repeated);
                let inserted = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(inserted.address.callsign, "MYDIGI");
                assert!(inserted.has_repeated);
                let exhausted = modified_packet.digipeaters.get(2).ok_or("missing digi 2")?;
                assert_eq!(exhausted.address.callsign, "WIDE2");
                assert_eq!(exhausted.address.ssid, 0);
                assert!(exhausted.has_repeated);
            }
            other => return Err(format!("expected relay on second hop, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn new_n_does_not_match_two_trailing_digits() {
        // The New-N digit is a single 1..=7. A callsign like "WIDE12" must
        // NOT match the "WIDE" base (it is neither the literal base nor a
        // base + single-digit form), so it drops.
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE12", 2)]);
        let t0 = Instant::now();
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    #[test]
    fn new_n_digit_zero_does_not_match() {
        // `WIDE0` is not a valid New-N requested-hop digit (1..=7), and the
        // callsign is not the literal base "WIDE", so it must drop.
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE0", 2)]);
        let t0 = Instant::now();
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    #[test]
    fn new_n_unrelated_base_does_not_match() {
        // A `WIDE2-2` entry must not be matched by an unrelated flood base.
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![],
            Some("CA".to_owned()),   // flood base CA
            Some("GATE".to_owned()), // trace base GATE
        );
        let packet = make_packet(vec![make_digi("WIDE2", 2)]);
        let t0 = Instant::now();
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    #[test]
    fn uiflood_last_hop_marks_used() -> TestResult {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("CA", 1)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "CA");
                assert!(d0.has_repeated);
                assert_eq!(d0.address.ssid, 0);
            }
            other => return Err(format!("expected Relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uiflood_zero_ssid_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("CA", 0)]);
        let t0 = Instant::now();

        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    // ---- UItrace tests ----

    #[test]
    fn uitrace_inserts_callsign_and_decrements() -> TestResult {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE", 3)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                // Our callsign inserted first, marked used.
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "MYDIGI");
                assert!(d0.has_repeated);
                assert_eq!(d0.address.ssid, 0);
                // Original entry with decremented hop.
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "WIDE");
                assert_eq!(d1.address.ssid, 2);
            }
            other => return Err(format!("expected Relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uitrace_last_hop_marks_exhausted() -> TestResult {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE", 1)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { modified_packet } => {
                assert_eq!(modified_packet.digipeaters.len(), 2);
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.callsign, "MYDIGI");
                assert!(d0.has_repeated);
                let d1 = modified_packet.digipeaters.get(1).ok_or("missing digi 1")?;
                assert_eq!(d1.address.callsign, "WIDE");
                assert!(d1.has_repeated);
                assert_eq!(d1.address.ssid, 0);
            }
            other => return Err(format!("expected Relay, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn uitrace_full_path_drops() -> TestResult {
        let mut config = make_config();
        // 8 digipeaters = maximum, can't insert another.
        let mut digis: Vec<RouteEntry> = (0..8).map(|i| make_digi("USED*", i)).collect();
        // Replace last one with an unused WIDE entry.
        let last = digis.get_mut(7).ok_or("missing digi 7")?;
        *last = make_digi("WIDE", 2);

        // But the first unused is at index 7, and there are already 8 entries.
        let packet = make_packet(digis);
        let t0 = Instant::now();
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
        Ok(())
    }

    // ---- Edge cases ----

    #[test]
    fn non_ui_frame_yields_not_ui_frame() {
        let mut config = make_config();
        let mut packet = make_packet(vec![make_digi("WIDE1", 1)]);
        packet.control = 0x01; // Not a UI frame.
        let t0 = Instant::now();

        assert_eq!(config.process(&packet, t0), DigiAction::NotUiFrame);
    }

    #[test]
    fn ui_frame_with_pf_bit_is_relayed() -> TestResult {
        // AX.25 UI frames may carry the P/F bit: control 0x13 is UI
        // exactly like 0x03 (`Ax25Control::from_byte` masks the P/F
        // bit before classifying). Real RF traffic includes 0x13 UI
        // frames; they must relay identically, not bounce as
        // NotUiFrame.
        let t0 = Instant::now();

        let mut plain_config = make_config();
        let plain = make_packet(vec![make_digi("WIDE1", 1)]);
        let plain_action = plain_config.process(&plain, t0);

        let mut pf_config = make_config();
        let mut pf = make_packet(vec![make_digi("WIDE1", 1)]);
        pf.control = 0x13; // UI with the P/F bit set.
        let pf_action = pf_config.process(&pf, t0);

        let DigiAction::Relay {
            modified_packet: plain_relay,
        } = plain_action
        else {
            return Err(format!("expected Relay for control 0x03, got {plain_action:?}").into());
        };
        let DigiAction::Relay {
            modified_packet: pf_relay,
        } = pf_action
        else {
            return Err(format!("expected Relay for control 0x13, got {pf_action:?}").into());
        };
        // Identical path modification; the frame keeps its own control
        // byte on retransmission.
        assert_eq!(pf_relay.digipeaters, plain_relay.digipeaters);
        assert_eq!(pf_relay.control, 0x13);
        Ok(())
    }

    #[test]
    fn empty_digipeater_path_drops() {
        let mut config = make_config();
        let packet = make_packet(vec![]);
        let t0 = Instant::now();

        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
    }

    #[test]
    fn case_insensitive_alias_match() -> TestResult {
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec!["wide1-1".to_owned()],
            None,
            None,
        );
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();

        match config.process(&packet, t0) {
            DigiAction::Relay { .. } => Ok(()),
            other => Err(format!("expected case-insensitive match, got {other:?}").into()),
        }
    }

    #[test]
    fn uitrace_priority_over_flood_when_both_configured() -> TestResult {
        // If both uiflood and uitrace are configured for different aliases,
        // the correct one should match.
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![],
            Some("CA".to_owned()),
            Some("WIDE".to_owned()),
        );

        let t0 = Instant::now();

        // UIflood packet (distinct info so dedup doesn't fire between cases).
        let mut flood_pkt = make_packet(vec![make_digi("CA", 2)]);
        flood_pkt.info = b"!3518.00N/08414.00W-flood".to_vec();
        match config.process(&flood_pkt, t0) {
            DigiAction::Relay { modified_packet } => {
                // Should NOT insert callsign (flood, not trace).
                assert_eq!(modified_packet.digipeaters.len(), 1);
                let d0 = modified_packet
                    .digipeaters
                    .first()
                    .ok_or("missing digi 0")?;
                assert_eq!(d0.address.ssid, 1);
            }
            other => return Err(format!("expected flood relay, got {other:?}").into()),
        }

        // UItrace packet.
        let mut trace_pkt = make_packet(vec![make_digi("WIDE", 2)]);
        trace_pkt.info = b"!3518.00N/08414.00W-trace".to_vec();
        match config.process(&trace_pkt, t0) {
            DigiAction::Relay { modified_packet } => {
                // Should insert callsign (trace).
                assert_eq!(modified_packet.digipeaters.len(), 2);
            }
            other => return Err(format!("expected trace relay, got {other:?}").into()),
        }
        Ok(())
    }

    // ---- Dedup cache tests ----

    #[test]
    fn duplicate_packet_within_window_is_dropped() {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();

        // First sighting → relay.
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
        assert_eq!(config.dedup_cache_len(), 1);

        // Second sighting within TTL → duplicate.
        let packet_2 = make_packet(vec![make_digi("WIDE1", 1)]);
        assert_eq!(config.process(&packet_2, t0), DigiAction::Duplicate);
    }

    #[test]
    fn dedup_distinguishes_different_info() {
        let mut config = make_config();
        let mut p1 = make_packet(vec![make_digi("WIDE1", 1)]);
        let mut p2 = make_packet(vec![make_digi("WIDE1", 1)]);
        p1.info = b"!3518.00N/08414.00W-one".to_vec();
        p2.info = b"!3518.00N/08414.00W-two".to_vec();
        let t0 = Instant::now();

        assert!(matches!(config.process(&p1, t0), DigiAction::Relay { .. }));
        // Different info → different hash → should relay.
        assert!(matches!(config.process(&p2, t0), DigiAction::Relay { .. }));
    }

    #[test]
    fn dedup_suppresses_within_ttl_admits_after_ttl() {
        // BUG 3 regression: the prune sweep is now amortized (throttled to
        // once per TTL), so duplicate detection must rely on the stored
        // timestamp, not on the sweep having run. A packet is a duplicate
        // for the full TTL window and admitted once past it — even though no
        // sweep necessarily ran in between.
        let mut config = make_config();
        config.dedup_ttl = Duration::from_secs(30);
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();

        // First sighting relays and records the hash.
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));

        // Still inside the 30 s window: duplicate, no sweep due yet.
        let within = t0 + Duration::from_secs(10);
        assert_eq!(config.process(&packet, within), DigiAction::Duplicate);
        assert_eq!(config.dedup_cache_len(), 1);

        // Past the TTL: the entry is stale, so the same packet is admitted
        // again (and re-recorded against the later timestamp).
        let after = t0 + Duration::from_secs(31);
        assert!(matches!(
            config.process(&packet, after),
            DigiAction::Relay { .. }
        ));
    }

    #[test]
    fn prune_is_amortized_not_run_every_call() {
        // The full `retain` sweep should fire at most once per TTL window.
        // Insert one entry, then make many `process` calls just inside the
        // window with distinct packets; the stale-reclamation pass should
        // not run on every call (we only observe its effect: the original
        // entry survives until the window elapses).
        let mut config = make_config();
        config.dedup_ttl = Duration::from_secs(30);
        let t0 = Instant::now();

        let first = make_packet(vec![make_digi("WIDE1", 1)]);
        assert!(matches!(
            config.process(&first, t0),
            DigiAction::Relay { .. }
        ));

        // A distinct packet 1 s later relays and adds a second entry; the
        // first entry must still be present (not yet expired, sweep or not).
        let mut second = make_packet(vec![make_digi("WIDE1", 1)]);
        second.info = b"!3518.00N/08414.00W-second".to_vec();
        assert!(matches!(
            config.process(&second, t0 + Duration::from_secs(1)),
            DigiAction::Relay { .. }
        ));
        assert_eq!(config.dedup_cache_len(), 2);

        // The very first packet, re-seen inside the window, is still a
        // duplicate (timestamp-aware check), confirming the entry was kept.
        assert_eq!(
            config.process(&first, t0 + Duration::from_secs(2)),
            DigiAction::Duplicate
        );
    }

    #[test]
    fn dedup_prunes_expired_entries() {
        let mut config = make_config();
        // Zero TTL so any "past" entry is instantly expired.
        config.dedup_ttl = Duration::from_secs(0);

        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
        // With zero TTL the previous entry is pruned, so the same packet
        // can be relayed again — pass the same instant to force the
        // pruning branch (`now.duration_since(t) < 0s` is false).
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
    }

    #[test]
    fn viscous_delay_defers_initial_relay() {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(5);
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        // With viscous_delay enabled, the first sighting is deferred.
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
        assert_eq!(config.drain_ready_viscous(t0).len(), 0);
    }

    #[test]
    fn viscous_delay_cancels_if_someone_else_relays() {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(5);
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        // Defer.
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
        // Same packet arrives again (someone else relayed).
        assert_eq!(config.process(&packet, t0), DigiAction::Duplicate);
        // Drained queue is empty because the pending relay was cancelled.
        assert_eq!(config.drain_ready_viscous(t0).len(), 0);
    }

    #[test]
    fn viscous_delay_zero_fires_immediately() {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(0);
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
    }

    #[test]
    fn own_callsign_with_h_bit_set_is_loop_detected() {
        let mut config = make_config(); // our callsign is MYDIGI
        // Packet already shows us as a used digi — must not be re-relayed.
        let packet = make_packet(vec![make_digi("MYDIGI*", 0), make_digi("WIDE2", 1)]);
        let t0 = Instant::now();
        assert_eq!(config.process(&packet, t0), DigiAction::LoopDetected);
    }

    #[test]
    fn own_callsign_unused_still_processes_first_entry() {
        let mut config = make_config();
        // Our callsign appears later in the path but the first entry is an
        // alias we should handle. The loop detector only trips on H-bit set.
        let packet = make_packet(vec![make_digi("WIDE1", 1), make_digi("MYDIGI", 0)]);
        let t0 = Instant::now();
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
    }

    // ---- Viscous drain timing ----

    #[test]
    fn drain_ready_viscous_returns_entries_past_delay() -> TestResult {
        let mut config = make_config();
        config.viscous_delay = Duration::from_secs(5);
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
        // Still inside the delay window: nothing ready yet.
        assert_eq!(config.drain_ready_viscous(t0).len(), 0);
        // Past the delay window: the pending relay is returned.
        let later = t0 + Duration::from_secs(6);
        let ready = config.drain_ready_viscous(later);
        assert_eq!(ready.len(), 1);
        let p = ready.first().ok_or("missing ready packet")?;
        // Our callsign was inserted by UIdigipeat substitution.
        let d0 = p.digipeaters.first().ok_or("missing digi 0")?;
        assert_eq!(d0.address.callsign, "MYDIGI");
        Ok(())
    }

    // ---- DigipeaterAlias ----

    #[test]
    fn alias_rejects_empty() {
        assert!(matches!(
            DigipeaterAlias::new(""),
            Err(AprsError::InvalidDigipeaterAlias(_))
        ));
    }

    #[test]
    fn alias_rejects_non_ascii() {
        assert!(matches!(
            DigipeaterAlias::new("CA\u{00E9}"),
            Err(AprsError::InvalidDigipeaterAlias(_))
        ));
    }

    #[test]
    fn alias_uppercases_input() -> TestResult {
        let a = DigipeaterAlias::new("wide1-1")?;
        assert_eq!(a.as_str(), "WIDE1-1");
        assert_eq!(format!("{a}"), "WIDE1-1");
        Ok(())
    }
}
