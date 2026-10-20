#![no_main]
//! Checks position-record wire preservation and decoded geographic bounds.

use kenwood_tmd750::memory::position::{POSITION_RECORD_SIZE, StoredPosition};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(bytes) = <[u8; POSITION_RECORD_SIZE]>::try_from(data)
        && let Ok(position) = StoredPosition::from_bytes(bytes)
    {
        assert_eq!(
            position.to_bytes(),
            bytes,
            "a decoded record keeps every byte"
        );
        assert!(
            position.latitude.degrees().abs() <= 90.0,
            "a decoded latitude lies within 90 degrees"
        );
        assert!(
            position.longitude.degrees().abs() <= 180.0,
            "a decoded longitude lies within 180 degrees"
        );
    }
});
