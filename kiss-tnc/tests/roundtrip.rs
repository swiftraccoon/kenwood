//! Pure KISS codec proptest round-trips.
//!
//! Split from `thd75/tests/kiss_roundtrip.rs` during PR 1 of the
//! KISS extraction. The AX.25 and APRS round-trip cases stay in
//! thd75 until those layers are extracted (PRs 2 and 3).

// Integration tests are separate compilation units and re-evaluate
// workspace deps. Suppress `unused_crate_dependencies` for the
// transitively-reachable lib dep that this test file does not use
// directly.
use thiserror as _;

use kiss_tnc::{KissFrame, decode_kiss_frame, encode_kiss_frame};
use proptest::prelude::*;

proptest! {
    #[test]
    fn kiss_codec_roundtrip(data in prop::collection::vec(any::<u8>(), 0..256)) {
        let frame = KissFrame {
            port: 0,
            command: 0x00,
            data: data.clone(),
        };
        let wire = encode_kiss_frame(&frame);
        let decoded = decode_kiss_frame(&wire)?;
        prop_assert_eq!(decoded.data, data);
    }
}
