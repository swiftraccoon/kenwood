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

use ax25_codec::{
    Ax25Address, Ax25Packet, Ax25Pid, Callsign, DigipeaterPath, MAX_DIGIPEATERS, RouteEntry, Ssid,
};

#[cfg(test)]
use ax25_codec::{Ax25Control, CommandResponse};

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
/// This newtype uses the AX.25 callsign character and width rules while
/// distinguishing a New-N alias base from an exact [`Ax25Address`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DigipeaterAlias(Callsign);

impl DigipeaterAlias {
    /// Maximum alias-base length, leaving one AX.25 callsign byte for the
    /// New-N requested-hop digit.
    pub const MAX_LEN: usize = Callsign::MAX_LEN - 1;

    /// Create a new alias base using AX.25 callsign rules.
    ///
    /// Input is accepted case-insensitively and normalized to uppercase.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidDigipeaterAlias`] if the base is empty,
    /// longer than five bytes, or contains anything other than ASCII letters
    /// and digits. Five bytes is the maximum because a New-N path callsign
    /// appends one requested-hop digit within AX.25's six-byte field.
    pub fn new(s: &str) -> Result<Self, AprsError> {
        let callsign = Callsign::new_case_insensitive(s).map_err(|_| {
            AprsError::InvalidDigipeaterAlias(
                "must be 1-5 ASCII letters or digits without an SSID suffix",
            )
        })?;
        if callsign.len() > Self::MAX_LEN {
            return Err(AprsError::InvalidDigipeaterAlias(
                "must leave room for the New-N hop digit (maximum 5 bytes)",
            ));
        }
        Ok(Self(callsign))
    }

    /// Return the alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Return the validated AX.25 callsign component used as the alias base.
    #[must_use]
    pub const fn as_callsign(&self) -> &Callsign {
        &self.0
    }
}

impl std::fmt::Display for DigipeaterAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
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
    callsign: Ax25Address,
    /// Exact `UIdigipeat` addresses (e.g., `WIDE1-1`). Relay if the path
    /// contains one, then replace it with our callsign + completion flag.
    uidigipeat_aliases: Vec<Ax25Address>,
    /// `UIflood` alias base (e.g., `"CA"`). Relay and decrement hop count.
    /// The SSID encodes the remaining hop count.
    uiflood_alias: Option<DigipeaterAlias>,
    /// `UItrace` alias base (e.g., `"WIDE"`). Relay, decrement hop count,
    /// and insert our callsign in the path.
    uitrace_alias: Option<DigipeaterAlias>,
    /// How long a recently-seen packet is remembered in the dedup cache.
    /// Defaults to [`DEFAULT_DEDUP_TTL`] (30 s). Zero disables deduplication.
    dedup_ttl: Duration,
    /// Viscous delay: how long to hold a relay candidate before
    /// actually transmitting it. `0` disables the feature (default).
    ///
    /// Viscous digis defer relay for a short window so that nearby
    /// full digipeaters have a chance to transmit first; if any other
    /// digi relays the packet within the window, the viscous digi
    /// cancels its own pending relay. This lets a fill-in digi stay
    /// quiet in well-covered areas while still providing coverage in
    /// RF gaps.
    viscous_delay: Duration,
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
    /// Build a new config with empty runtime caches and default timing.
    #[must_use]
    pub fn new(
        callsign: Ax25Address,
        uidigipeat_aliases: Vec<Ax25Address>,
        uiflood_alias: Option<DigipeaterAlias>,
        uitrace_alias: Option<DigipeaterAlias>,
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

    /// Return this station's digipeater address.
    #[must_use]
    pub const fn callsign(&self) -> &Ax25Address {
        &self.callsign
    }

    /// Replace this station's digipeater address.
    pub fn set_callsign(&mut self, callsign: Ax25Address) {
        self.callsign = callsign;
    }

    /// Return the exact addresses handled by `UIdigipeat`.
    #[must_use]
    pub fn uidigipeat_aliases(&self) -> &[Ax25Address] {
        &self.uidigipeat_aliases
    }

    /// Replace the exact addresses handled by `UIdigipeat`.
    pub fn set_uidigipeat_aliases(&mut self, aliases: Vec<Ax25Address>) {
        self.uidigipeat_aliases = aliases;
    }

    /// Return the `UIflood` New-N alias base, if configured.
    #[must_use]
    pub const fn uiflood_alias(&self) -> Option<&DigipeaterAlias> {
        self.uiflood_alias.as_ref()
    }

    /// Replace or disable the `UIflood` New-N alias base.
    pub fn set_uiflood_alias(&mut self, alias: Option<DigipeaterAlias>) {
        self.uiflood_alias = alias;
    }

    /// Return the `UItrace` New-N alias base, if configured.
    #[must_use]
    pub const fn uitrace_alias(&self) -> Option<&DigipeaterAlias> {
        self.uitrace_alias.as_ref()
    }

    /// Replace or disable the `UItrace` New-N alias base.
    pub fn set_uitrace_alias(&mut self, alias: Option<DigipeaterAlias>) {
        self.uitrace_alias = alias;
    }

    /// Return the rolling deduplication window.
    ///
    /// Zero means deduplication is disabled.
    #[must_use]
    pub const fn dedup_ttl(&self) -> Duration {
        self.dedup_ttl
    }

    /// Set the rolling deduplication window.
    ///
    /// Zero is valid and disables deduplication. Switching to zero clears
    /// existing dedup state immediately.
    pub fn set_dedup_ttl(&mut self, dedup_ttl: Duration) {
        self.dedup_ttl = dedup_ttl;
        self.last_prune = None;
        if dedup_ttl.is_zero() {
            self.dedup_cache.clear();
        }
    }

    /// Return the viscous relay delay.
    ///
    /// Zero means relay candidates are returned immediately.
    #[must_use]
    pub const fn viscous_delay(&self) -> Duration {
        self.viscous_delay
    }

    /// Set the viscous relay delay.
    ///
    /// Zero disables deferral. Already-pending candidates become eligible
    /// on the next non-regressing [`Self::drain_ready_viscous`] call.
    pub const fn set_viscous_delay(&mut self, viscous_delay: Duration) {
        self.viscous_delay = viscous_delay;
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
            if now
                .checked_duration_since(t)
                .is_some_and(|elapsed| elapsed >= delay)
            {
                ready.push(p);
                // Record this relay in the dedup cache to prevent
                // re-relaying if the packet comes around again.
                if !self.dedup_ttl.is_zero() {
                    let _prev = self.dedup_cache.insert(k, now);
                }
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
    /// classify as Unnumbered Information (P/F bit permitted, so both
    /// `0x03` and `0x13` are UI) or PID != 0xF0). APRS uses only UI
    /// frames, so this is effectively a pass-through.
    NotUiFrame,
    /// Loop detected: our own callsign is already in the used path.
    LoopDetected,
    /// Duplicate packet: we already relayed this one within the TTL
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
    /// 2. Own-callsign loop detection: if our callsign appears anywhere
    ///    in the digipeater path with the H-bit set, the packet has already
    ///    been through us and we must drop it to prevent routing loops.
    /// 3. Dedup cache lookup: if we've relayed a packet with the same
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
        if !packet.is_ui() || packet.protocol_identifier() != Some(Ax25Pid::NoLayer3) {
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
        // TTL; a stale entry that has not yet been reclaimed is ignored.
        self.prune_dedup(now);
        let packet_hash = hash_packet_identity(packet);
        if self.dedup_cache.get(&packet_hash).is_some_and(|&t| {
            now.checked_duration_since(t)
                .is_none_or(|elapsed| elapsed < self.dedup_ttl)
        }) {
            return DigiAction::Duplicate;
        }

        // --- 3a. Viscous cancellation ---
        // If we have a pending viscous relay for this packet and the
        // packet arrives again, it means someone else relayed it. Drop
        // the pending entry and suppress our own relay.
        if !self.viscous_delay.is_zero() && self.pending_viscous.remove(&packet_hash).is_some() {
            if !self.dedup_ttl.is_zero() {
                let _prev = self.dedup_cache.insert(packet_hash, now);
            }
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
        // matches a validated full address exactly. None of these checks
        // allocate per packet.
        let callsign = digi.address.callsign.as_str();
        let hops_remaining = digi.address.ssid.get() > 0;
        let action =
            if self
                .uidigipeat_aliases
                .iter()
                .any(|alias| &digi.address == alias)
            {
                apply_uidigipeat(&self.callsign, packet, first_unused)
            } else if self.uiflood_alias.as_ref().is_some_and(|alias| {
                hops_remaining && matches_new_n_alias(callsign, alias.as_str())
            }) {
                apply_uiflood(packet, first_unused)
            } else if self.uitrace_alias.as_ref().is_some_and(|alias| {
                hops_remaining && matches_new_n_alias(callsign, alias.as_str())
            }) {
                apply_uitrace(&self.callsign, packet, first_unused)
            } else {
                DigiAction::Drop
            };

        // --- 5. Record successful relay in dedup cache ---
        if let DigiAction::Relay {
            ref modified_packet,
        } = action
        {
            if !self.viscous_delay.is_zero() {
                // Defer the relay: hold it in the viscous queue. The
                // dedup cache is only populated once we actually
                // transmit (in `drain_ready_viscous`).
                let _prev = self
                    .pending_viscous
                    .insert(packet_hash, (now, modified_packet.clone()));
                return DigiAction::Drop;
            }
            if !self.dedup_ttl.is_zero() {
                let _previous = self.dedup_cache.insert(packet_hash, now);
            }
        }

        action
    }

    /// Remove dedup entries older than [`Self::dedup_ttl`], at most once per
    /// TTL window.
    ///
    /// This is a memory-reclamation pass only; duplicate detection in
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
        self.dedup_cache.retain(|_, t| {
            now.checked_duration_since(*t)
                .is_none_or(|elapsed| elapsed < ttl)
        });
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
/// Uses [`DefaultHasher`] as an in-process cache-key compressor. A hasher
/// created directly with [`DefaultHasher::new`] is deterministically
/// initialized, not randomly seeded; its algorithm and output are not a
/// stable persistence or interchange format. These hashes are never exposed
/// or stored beyond the current process.
fn hash_packet_identity(packet: &Ax25Packet) -> u64 {
    let mut h = DefaultHasher::new();
    packet.source.callsign.as_str().hash(&mut h);
    packet.source.ssid.get().hash(&mut h);
    packet.destination.callsign.as_str().hash(&mut h);
    packet.destination.ssid.get().hash(&mut h);
    packet.information().hash(&mut h);
    h.finish()
}

/// Check whether our callsign appears in the digipeater path with the
/// has-been-repeated bit set. If so, the packet has already passed through
/// this station and relaying it again would create a routing loop.
fn own_callsign_already_relayed(own: &Ax25Address, path: &DigipeaterPath) -> bool {
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
    let new_ssid = digi.address.ssid.saturating_decrement();
    let callsign = digi.address.callsign.clone();

    let mut modified = packet.clone();
    let Some(slot) = modified.digipeaters.get_mut(idx) else {
        return DigiAction::Drop;
    };
    if new_ssid == Ssid::ZERO {
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
    // after `modified.digipeaters.try_insert` the indices shift and we can
    // no longer borrow from the original slice without re-indexing.
    let Some(source_digi) = packet.digipeaters.get(idx) else {
        return DigiAction::Drop;
    };
    let alias_callsign = source_digi.address.callsign.clone();
    let new_ssid = source_digi.address.ssid.saturating_decrement();

    let mut modified = packet.clone();

    // Insert our callsign (marked as used) before the current entry.
    if modified
        .digipeaters
        .try_insert(
            idx,
            RouteEntry {
                address: callsign.clone(),
                has_repeated: true,
            },
        )
        .is_err()
    {
        return DigiAction::Drop;
    }

    // The original entry shifted to idx+1; update its hop count.
    let trace_idx = idx + 1;
    let Some(slot) = modified.digipeaters.get_mut(trace_idx) else {
        return DigiAction::Drop;
    };
    if new_ssid == Ssid::ZERO {
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
/// `callsign == "WIDE2"`, `ssid == 2`, **not** `callsign == "WIDE"`.
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

    fn make_alias(base: &str) -> DigipeaterAlias {
        DigipeaterAlias::new(base)
            .unwrap_or_else(|_| unreachable!("test fixture alias base is statically valid"))
    }

    fn make_packet(digipeaters: Vec<RouteEntry>) -> Ax25Packet {
        make_packet_with_poll_final(digipeaters, false)
    }

    fn make_packet_with_poll_final(digipeaters: Vec<RouteEntry>, poll_final: bool) -> Ax25Packet {
        Ax25Packet::unnumbered_information(
            make_addr("N0CALL", 7),
            make_addr("APK005", 0),
            DigipeaterPath::new(digipeaters)
                .unwrap_or_else(|_| unreachable!("test fixtures use at most eight digipeaters")),
            CommandResponse::Command,
            poll_final,
            Ax25Pid::NoLayer3,
            b"!3518.00N/08414.00W-test".to_vec(),
        )
    }

    fn make_config() -> DigipeaterConfig {
        DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![make_addr("WIDE1", 1)],
            Some(make_alias("CA")),
            Some(make_alias("WIDE")),
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
    fn uidigipeat_requires_an_exact_address_match() {
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![make_addr("WIDE1", 1)],
            None,
            None,
        );
        let packet = make_packet(vec![make_digi("WIDE1", 2)]);

        assert_eq!(config.process(&packet, Instant::now()), DigiAction::Drop);
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
        let packet = Ax25Packet::unnumbered_information(
            make_addr("N0CALL", 7),
            make_addr("APK005", 0),
            DigipeaterPath::new(digipeaters)?,
            CommandResponse::Command,
            false,
            Ax25Pid::NoLayer3,
            info.to_vec(),
        );
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
        // would match with higher precedence; see the dedicated precedence
        // test.) Use a config whose only alias family is the "WIDE" trace
        // base so this exclusively exercises the New-N UItrace last hop.
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![],
            None,
            Some(make_alias("WIDE")),
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
            Some(make_alias("CA")),   // flood base CA
            Some(make_alias("GATE")), // trace base GATE
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
        let ui = make_packet(vec![make_digi("WIDE1", 1)]);
        let packet = Ax25Packet::try_new(
            ui.source,
            ui.destination,
            ui.digipeaters,
            ui.command_or_response,
            Ax25Control::from_byte(0x01),
            None,
            Vec::new(),
        )
        .unwrap_or_else(|_| unreachable!("RR without PID or information is valid"));
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
        let pf = make_packet_with_poll_final(vec![make_digi("WIDE1", 1)], true);
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
        assert_eq!(pf_relay.control_byte(), 0x13);
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
    fn config_accessors_and_setters_preserve_typed_values() {
        let mut config = make_config();
        assert_eq!(config.callsign(), &make_addr("MYDIGI", 0));
        assert_eq!(config.uidigipeat_aliases(), &[make_addr("WIDE1", 1)]);
        assert_eq!(
            config.uiflood_alias().map(DigipeaterAlias::as_str),
            Some("CA")
        );
        assert_eq!(
            config.uitrace_alias().map(DigipeaterAlias::as_str),
            Some("WIDE")
        );
        assert_eq!(config.dedup_ttl(), DEFAULT_DEDUP_TTL);
        assert_eq!(config.viscous_delay(), DEFAULT_VISCOUS_DELAY);

        config.set_callsign(make_addr("NEW", 1));
        config.set_uidigipeat_aliases(vec![make_addr("RELAY", 0)]);
        config.set_uiflood_alias(None);
        config.set_uitrace_alias(Some(make_alias("TRACE")));
        config.set_dedup_ttl(Duration::from_secs(12));
        config.set_viscous_delay(Duration::from_secs(3));

        assert_eq!(config.callsign(), &make_addr("NEW", 1));
        assert_eq!(config.uidigipeat_aliases(), &[make_addr("RELAY", 0)]);
        assert!(config.uiflood_alias().is_none());
        assert_eq!(
            config.uitrace_alias().map(DigipeaterAlias::as_str),
            Some("TRACE")
        );
        assert_eq!(config.dedup_ttl(), Duration::from_secs(12));
        assert_eq!(config.viscous_delay(), Duration::from_secs(3));
    }

    #[test]
    fn case_insensitive_alias_match() -> TestResult {
        let mut config = DigipeaterConfig::new(
            make_addr("MYDIGI", 0),
            vec![make_addr("wide1", 1)],
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
            Some(make_alias("CA")),
            Some(make_alias("WIDE")),
        );

        let t0 = Instant::now();

        // UIflood packet (distinct info so dedup doesn't fire between cases).
        let mut flood_pkt = make_packet(vec![make_digi("CA", 2)]);
        *flood_pkt
            .information_mut()
            .unwrap_or_else(|| unreachable!("UI frame has an information field")) =
            b"!3518.00N/08414.00W-flood".to_vec();
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
        *trace_pkt
            .information_mut()
            .unwrap_or_else(|| unreachable!("UI frame has an information field")) =
            b"!3518.00N/08414.00W-trace".to_vec();
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
        *p1.information_mut()
            .unwrap_or_else(|| unreachable!("UI frame has an information field")) =
            b"!3518.00N/08414.00W-one".to_vec();
        *p2.information_mut()
            .unwrap_or_else(|| unreachable!("UI frame has an information field")) =
            b"!3518.00N/08414.00W-two".to_vec();
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
        // for the full TTL window and admitted once past it, even though no
        // sweep necessarily ran in between.
        let mut config = make_config();
        config.set_dedup_ttl(Duration::from_secs(30));
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
        config.set_dedup_ttl(Duration::from_secs(30));
        let t0 = Instant::now();

        let first = make_packet(vec![make_digi("WIDE1", 1)]);
        assert!(matches!(
            config.process(&first, t0),
            DigiAction::Relay { .. }
        ));

        // A distinct packet 1 s later relays and adds a second entry; the
        // first entry must still be present (not yet expired, sweep or not).
        let mut second = make_packet(vec![make_digi("WIDE1", 1)]);
        *second
            .information_mut()
            .unwrap_or_else(|| unreachable!("UI frame has an information field")) =
            b"!3518.00N/08414.00W-second".to_vec();
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
    fn zero_dedup_ttl_disables_duplicate_suppression_and_clears_state() {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
        assert_eq!(config.dedup_cache_len(), 1);

        config.set_dedup_ttl(Duration::ZERO);
        assert_eq!(config.dedup_ttl(), Duration::ZERO);
        assert_eq!(config.dedup_cache_len(), 0);

        // With dedup explicitly disabled, repeated packets relay even at the
        // same instant and no cache entries accumulate.
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
        assert!(matches!(
            config.process(&packet, t0),
            DigiAction::Relay { .. }
        ));
        assert_eq!(config.dedup_cache_len(), 0);
    }

    #[test]
    fn regressing_process_time_is_conservatively_treated_as_duplicate() {
        let mut config = make_config();
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let earlier = Instant::now();
        let later = earlier + Duration::from_secs(10);

        assert!(matches!(
            config.process(&packet, later),
            DigiAction::Relay { .. }
        ));
        assert_eq!(config.process(&packet, earlier), DigiAction::Duplicate);
    }

    #[test]
    fn viscous_delay_defers_initial_relay() {
        let mut config = make_config();
        config.set_viscous_delay(Duration::from_secs(5));
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let t0 = Instant::now();
        // With viscous_delay enabled, the first sighting is deferred.
        assert_eq!(config.process(&packet, t0), DigiAction::Drop);
        assert_eq!(config.drain_ready_viscous(t0).len(), 0);
    }

    #[test]
    fn viscous_delay_cancels_if_someone_else_relays() {
        let mut config = make_config();
        config.set_viscous_delay(Duration::from_secs(5));
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
    fn regressing_viscous_drain_time_keeps_candidate_pending() {
        let mut config = make_config();
        config.set_viscous_delay(Duration::from_secs(5));
        let packet = make_packet(vec![make_digi("WIDE1", 1)]);
        let earlier = Instant::now();
        let queued_at = earlier + Duration::from_secs(10);

        assert_eq!(config.process(&packet, queued_at), DigiAction::Drop);
        assert!(config.drain_ready_viscous(earlier).is_empty());
        assert_eq!(
            config
                .drain_ready_viscous(queued_at + Duration::from_secs(5))
                .len(),
            1
        );
    }

    #[test]
    fn viscous_delay_zero_fires_immediately() {
        let mut config = make_config();
        config.set_viscous_delay(Duration::ZERO);
        assert_eq!(config.viscous_delay(), Duration::ZERO);
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
        // Packet already shows us as a used digi; must not be re-relayed.
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
        config.set_viscous_delay(Duration::from_secs(5));
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
    fn alias_rejects_ssid_suffix_punctuation_and_excess_width() {
        for invalid in ["WIDE1-1", "WIDE_", "ABCDEF", "TOOLONG"] {
            assert!(matches!(
                DigipeaterAlias::new(invalid),
                Err(AprsError::InvalidDigipeaterAlias(_))
            ));
        }
    }

    #[test]
    fn alias_uppercases_valid_ax25_base() -> TestResult {
        let alias = DigipeaterAlias::new("trace")?;
        assert_eq!(alias.as_str(), "TRACE");
        assert_eq!(alias.as_callsign().as_str(), "TRACE");
        assert_eq!(alias.as_str().len(), DigipeaterAlias::MAX_LEN);
        assert_eq!(format!("{alias}"), "TRACE");
        Ok(())
    }
}
