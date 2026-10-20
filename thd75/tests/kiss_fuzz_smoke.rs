//! Smoke fuzzing for the AX.25 / APRS parsers.
//!
//! The AX.25 and APRS parsers receive deterministic pseudo-random byte
//! sequences. A panic fails the test; fixed seeds reproduce its input.

use aprs::{AprsData, AprsPosition};
use ax25_codec::Ax25Packet;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs_is as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kenwood_schema as _;
use kenwood_thd75 as _;
use kenwood_transport as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

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
fn fuzz_parse_ax25_no_panic() {
    let mut rng = Xor32(0xDEAD_BEEF);
    for size in 0..=128 {
        for _ in 0..50 {
            let mut buf = vec![0u8; size];
            rng.fill(&mut buf);
            drop(Ax25Packet::from_bytes(&buf));
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
            drop(AprsData::from_bytes(&buf));
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
            drop(AprsPosition::from_bytes(&buf));
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
        drop(AprsPosition::from_bytes(tpl));
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
            drop(AprsData::from_bytes(&buf));
        }
    }
}
