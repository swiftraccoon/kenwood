//! Smoke fuzzing for the KISS codec.
//!
//! One-shot and streaming decoders receive deterministic pseudo-random
//! byte sequences. A panic fails the test; fixed seeds reproduce its input.

// Integration tests are separate compilation units that re-evaluate
// workspace deps; acknowledge the transitively-reachable lib deps these
// tests do not name directly.
use proptest as _;
use thiserror as _;

use kiss_tnc::{KissDecoder, KissFrame};

/// Tiny xorshift32 RNG: deterministic, no `rand` dependency.
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
            // The result does not matter: decoding arbitrary input must
            // return a `Result`, never panic. `matches!` consumes the
            // value and documents the "just don't panic" intent.
            assert!(
                matches!(KissFrame::from_bytes(&buf), Ok(_) | Err(_)),
                "decoder must return a Result, not panic",
            );
        }
    }
}

#[test]
fn fuzz_kiss_decoder_stream_no_panic() {
    let mut rng = Xor32(0x5EED_1234);
    for size in 0..=128 {
        for _ in 0..50 {
            let mut buf = vec![0u8; size];
            rng.fill(&mut buf);
            let mut decoder = KissDecoder::new();
            decoder.push(&buf);
            // Drain the decoder. Every call must return a `Result`, never
            // panic; the loop is bounded because each non-`None` result
            // consumes buffer bytes.
            for _ in 0..=size {
                if matches!(decoder.next_frame(), Ok(None)) {
                    break;
                }
            }
            let remaining = decoder.next_frame();
            assert!(
                matches!(remaining, Ok(None)),
                "decoder must consume complete frames and errors: {remaining:?}",
            );
        }
    }
}
