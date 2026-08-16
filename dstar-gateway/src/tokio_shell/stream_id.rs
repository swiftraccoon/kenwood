//! Clock-seeded stream-ID generation for fresh TX streams.
//!
//! The sans-io core deliberately owns no clock, so the seed comes from
//! this shell layer: [`fresh_stream_id`] hashes the wall clock into
//! [`StreamId::from_seed`], which maps the protocol-reserved zero to a
//! valid ID. Stream IDs only need to differ from the previous stream
//! on the same link (receivers key their per-stream state on the ID
//! changing), so a cheap time seed is sufficient; no cryptographic
//! generator is required.

use std::time::{SystemTime, UNIX_EPOCH};

use dstar_gateway_core::types::StreamId;

/// Generate a stream ID for a fresh TX stream, seeded from the wall
/// clock (sub-second nanoseconds XOR whole seconds).
///
/// The zero seed the protocol reserves is mapped to a valid ID by
/// [`StreamId::from_seed`], so this never fails and never needs a
/// retry loop.
#[must_use]
pub fn fresh_stream_id() -> StreamId {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "stream IDs are 16-bit on the wire; folding the clock down to the low 16 \
                  bits is the seed derivation, not an accidental loss"
    )]
    let seed = (elapsed.subsec_nanos() ^ elapsed.as_secs() as u32) as u16;
    StreamId::from_seed(seed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fresh_stream_ids_vary_across_draws() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let _newly_seen = seen.insert(super::fresh_stream_id().get());
        }
        assert!(
            seen.len() > 1,
            "100 clock-seeded draws must not all collide"
        );
    }
}
