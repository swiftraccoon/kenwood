//! Smoke fuzzing for the KISS codec.
//!
//! Split from `thd75/tests/kiss_fuzz_smoke.rs` during PR 1 of the
//! KISS extraction. Only the pure-KISS decode fuzz case lives here;
//! the AX.25 and APRS fuzz cases stay in thd75 until those layers
//! are extracted (PRs 2 and 3).
//!
//! This is not a real `cargo-fuzz` harness — that requires nightly.
//! Instead, we feed the decoder a large number of pseudo-random byte
//! sequences and assert that none of them panic. The seed is fixed
//! so the test is reproducible.
//!
//! For real fuzzing (libfuzzer/AFL), the same body can be lifted into
//! a `fuzz_targets/` crate.

// Integration tests are separate compilation units and re-evaluate
// workspace deps. Suppress `unused_crate_dependencies` for the
// transitively-reachable lib dep that this test file does not use
// directly.
use proptest as _;
use thiserror as _;

use kiss_tnc::decode_kiss_frame;

/// Tiny xorshift32 RNG — deterministic, no `rand` dependency.
struct Xor32(u32);

impl Xor32 {
    const fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(4) {
            let bytes = self.next_u32().to_le_bytes();
            for (dst, src) in chunk.iter_mut().zip(bytes.iter()) {
                *dst = *src;
            }
        }
    }
}

#[test]
fn fuzz_kiss_decode_no_panic() {
    let mut rng = Xor32(0xC0FF_EE42);
    for size in 0..=128 {
        for _ in 0..50 {
            let mut buf = vec![0u8; size];
            rng.fill(&mut buf);
            // Result doesn't matter — we just want to make sure
            // decoding doesn't panic on arbitrary input. `matches!`
            // consumes the value and documents the "just don't panic"
            // intent while satisfying `let_underscore_drop`.
            assert!(
                matches!(decode_kiss_frame(&buf), Ok(_) | Err(_)),
                "decoder must return a Result, not panic"
            );
        }
    }
}
