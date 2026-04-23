//! Smoke fuzzing for the AX.25 / APRS parsers.
//!
//! Not a real `cargo-fuzz` harness — that requires nightly. Instead,
//! we feed the parsers a large number of pseudo-random byte sequences
//! and assert that none of them panic. The seed is fixed so the test
//! is reproducible.
//!
//! The pure-KISS decode fuzz case lives in `kiss-tnc/tests/fuzz_smoke.rs`
//! (extracted in PR 1 of the KISS / AX.25 / APRS split). AX.25 and
//! APRS cases stay here until those layers are extracted in PRs 2
//! and 3.
//!
//! For real fuzzing (libfuzzer/AFL), the same body can be lifted into
//! a `fuzz_targets/` crate.

use aprs::{parse_aprs_data, parse_aprs_position};
use ax25_codec::parse_ax25;

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
fn fuzz_parse_ax25_no_panic() {
    let mut rng = Xor32(0xDEAD_BEEF);
    for size in 0..=128 {
        for _ in 0..50 {
            let mut buf = vec![0u8; size];
            rng.fill(&mut buf);
            let _ = parse_ax25(&buf);
        }
    }
}

#[test]
fn fuzz_parse_aprs_data_no_panic() {
    let mut rng = Xor32(0xCAFE_BABE);
    for size in 0..=128 {
        for _ in 0..50 {
            let mut buf = vec![0u8; size];
            rng.fill(&mut buf);
            let _ = parse_aprs_data(&buf);
        }
    }
}

#[test]
fn fuzz_parse_aprs_position_no_panic() {
    let mut rng = Xor32(0x1234_5678);
    for size in 0..=64 {
        for _ in 0..50 {
            let mut buf = vec![0u8; size];
            rng.fill(&mut buf);
            // Force the data type identifier to one of the position
            // identifiers so we exercise the position parser path.
            if let Some(first) = buf.first_mut() {
                *first = match *first % 4 {
                    0 => b'!',
                    1 => b'=',
                    2 => b'/',
                    _ => b'@',
                };
            }
            let _ = parse_aprs_position(&buf);
        }
    }
}

#[test]
fn fuzz_parse_aprs_position_with_spaces() {
    // APRS 1.0.1 §8.1.6 position ambiguity uses spaces. Make sure we
    // don't panic when spaces appear in unexpected positions.
    let templates: &[&[u8]] = &[
        b"!49  .  N/072  .  W>",
        b"!4903.5 N/07201.7 W>",
        b"!4903.50N/07201.75W ",
        b"!49 3.50N/07201.75W>",
        b"!4 03.50N/07201.75W>",
    ];
    for tpl in templates {
        let _ = parse_aprs_position(tpl);
    }
}

#[test]
fn fuzz_parse_aprs_data_known_first_bytes() {
    // For each data-type identifier, generate random tail bytes and
    // verify the dispatcher doesn't panic.
    let prefixes: &[u8] = b"!=/@:>;)_T?\\$<%}{,[`'";
    let mut rng = Xor32(0xAAAA_5555);
    for &prefix in prefixes {
        for size in 1..=64 {
            let mut buf = vec![0u8; size];
            if let Some(first) = buf.first_mut() {
                *first = prefix;
            }
            if size > 1
                && let Some(tail) = buf.get_mut(1..)
            {
                rng.fill(tail);
            }
            let _ = parse_aprs_data(&buf);
        }
    }
}
