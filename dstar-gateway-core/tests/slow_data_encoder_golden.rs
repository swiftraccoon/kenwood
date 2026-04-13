//! Golden-byte regression tests for `encode_text_message`.
//!
//! These vectors pin the exact scrambled output of the encoder against
//! hand-computed byte sequences derived from the D-STAR slow-data
//! text-block algorithm (ircDDBGateway reference). Any byte-level drift
//! in the encoder will fail these tests loudly.
//!
//! Method for hand-computing a vector for input `"<text>"`:
//!   1. Pad/truncate to exactly 20 bytes with ASCII space.
//!   2. For each block index i ∈ 0..=3:
//!      a. Let `chunk = padded[i*5..i*5+5]` (5 bytes).
//!      b. Let `block = [0x40 | i, chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]]`.
//!      c. half1 = block[0..3] XOR [0x70, 0x4F, 0x93].
//!      d. half2 = block[3..6] XOR [0x70, 0x4F, 0x93].

// Integration tests are separate compilation units — each one must
// silence `unused_crate_dependencies` for workspace crates it doesn't
// directly reference beyond `src/lib.rs`.
use proptest as _;
use static_assertions as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

use dstar_gateway_core::slowdata::encode_text_message;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn golden_hi_padded() -> TestResult {
    // "Hi" padded to "Hi                  ":
    //   Block 0: [0x40, 'H', 'i', ' ', ' ', ' '] = [0x40, 0x48, 0x69, 0x20, 0x20, 0x20]
    //     half1 = [0x40^0x70, 0x48^0x4F, 0x69^0x93] = [0x30, 0x07, 0xFA]
    //     half2 = [0x20^0x70, 0x20^0x4F, 0x20^0x93] = [0x50, 0x6F, 0xB3]
    //   Block 1: [0x41, ' ', ' ', ' ', ' ', ' '] = [0x41, 0x20, 0x20, 0x20, 0x20, 0x20]
    //     half1 = [0x41^0x70, 0x20^0x4F, 0x20^0x93] = [0x31, 0x6F, 0xB3]
    //     half2 = [0x50, 0x6F, 0xB3]
    //   Block 2: type=0x42, rest spaces
    //     half1 = [0x32, 0x6F, 0xB3]
    //     half2 = [0x50, 0x6F, 0xB3]
    //   Block 3: type=0x43, rest spaces
    //     half1 = [0x33, 0x6F, 0xB3]
    //     half2 = [0x50, 0x6F, 0xB3]
    let out = encode_text_message("Hi");
    assert_eq!(out.len(), 8);
    assert_eq!(*out.first().ok_or("index 0 in range")?, [0x30, 0x07, 0xFA]);
    assert_eq!(*out.get(1).ok_or("index 1 in range")?, [0x50, 0x6F, 0xB3]);
    assert_eq!(*out.get(2).ok_or("index 2 in range")?, [0x31, 0x6F, 0xB3]);
    assert_eq!(*out.get(3).ok_or("index 3 in range")?, [0x50, 0x6F, 0xB3]);
    assert_eq!(*out.get(4).ok_or("index 4 in range")?, [0x32, 0x6F, 0xB3]);
    assert_eq!(*out.get(5).ok_or("index 5 in range")?, [0x50, 0x6F, 0xB3]);
    assert_eq!(*out.get(6).ok_or("index 6 in range")?, [0x33, 0x6F, 0xB3]);
    assert_eq!(*out.get(7).ok_or("index 7 in range")?, [0x50, 0x6F, 0xB3]);
    Ok(())
}

#[test]
fn golden_empty_returns_empty() {
    assert!(encode_text_message("").is_empty());
}

#[test]
fn golden_exactly_20_chars_preserves_all_chars() -> TestResult {
    // All-'A' 20-char message. Each block: [0x40|i, 'A','A','A','A','A']
    //   block[0] = 0x40|i
    //   half1 = [(0x40|i)^0x70, 'A'^0x4F, 'A'^0x93] = [0x30|i, 0x0E, 0xD2]
    //   half2 = ['A'^0x70, 'A'^0x4F, 'A'^0x93]      = [0x31, 0x0E, 0xD2]
    //   (0x41 = 'A')
    let out = encode_text_message("AAAAAAAAAAAAAAAAAAAA");
    assert_eq!(out.len(), 8);
    assert_eq!(*out.first().ok_or("index 0 in range")?, [0x30, 0x0E, 0xD2]);
    assert_eq!(*out.get(1).ok_or("index 1 in range")?, [0x31, 0x0E, 0xD2]);
    assert_eq!(*out.get(2).ok_or("index 2 in range")?, [0x31, 0x0E, 0xD2]);
    assert_eq!(*out.get(3).ok_or("index 3 in range")?, [0x31, 0x0E, 0xD2]);
    assert_eq!(*out.get(4).ok_or("index 4 in range")?, [0x32, 0x0E, 0xD2]);
    assert_eq!(*out.get(5).ok_or("index 5 in range")?, [0x31, 0x0E, 0xD2]);
    assert_eq!(*out.get(6).ok_or("index 6 in range")?, [0x33, 0x0E, 0xD2]);
    assert_eq!(*out.get(7).ok_or("index 7 in range")?, [0x31, 0x0E, 0xD2]);
    Ok(())
}

#[test]
fn golden_long_input_truncated_to_first_20_chars() -> TestResult {
    // First 20 chars of "1234567890ABCDEFGHIJKLMN" = "1234567890ABCDEFGHIJ"
    // Spot-check block 0: [0x40, '1','2','3','4','5']
    //   half1 = [0x40^0x70, '1'^0x4F, '2'^0x93] = [0x30, 0x7E, 0xA1]
    //   half2 = ['3'^0x70, '4'^0x4F, '5'^0x93] = [0x43, 0x7B, 0xA6]
    let out = encode_text_message("1234567890ABCDEFGHIJKLMN");
    assert_eq!(out.len(), 8);
    assert_eq!(*out.first().ok_or("index 0 in range")?, [0x30, 0x7E, 0xA1]);
    assert_eq!(*out.get(1).ok_or("index 1 in range")?, [0x43, 0x7B, 0xA6]);
    Ok(())
}

#[test]
fn golden_alphanumeric_roundtrip_exact() -> TestResult {
    // "HELLO WORLD" padded to "HELLO WORLD         "
    // Block 0: [0x40, 'H','E','L','L','O'] = [0x40, 0x48, 0x45, 0x4C, 0x4C, 0x4F]
    //   half1 = [0x40^0x70, 0x48^0x4F, 0x45^0x93] = [0x30, 0x07, 0xD6]
    //   half2 = [0x4C^0x70, 0x4C^0x4F, 0x4F^0x93] = [0x3C, 0x03, 0xDC]
    // Block 1: [0x41, ' ','W','O','R','L']
    //   half1 = [0x41^0x70, 0x20^0x4F, 0x57^0x93] = [0x31, 0x6F, 0xC4]
    //   half2 = [0x4F^0x70, 0x52^0x4F, 0x4C^0x93] = [0x3F, 0x1D, 0xDF]
    // Block 2: [0x42, 'D',' ',' ',' ',' ']
    //   half1 = [0x42^0x70, 0x44^0x4F, 0x20^0x93] = [0x32, 0x0B, 0xB3]
    //   half2 = [0x20^0x70, 0x20^0x4F, 0x20^0x93] = [0x50, 0x6F, 0xB3]
    // Block 3: [0x43, ' ',' ',' ',' ',' ']
    //   half1 = [0x33, 0x6F, 0xB3]
    //   half2 = [0x50, 0x6F, 0xB3]
    let out = encode_text_message("HELLO WORLD");
    assert_eq!(out.len(), 8);
    assert_eq!(*out.first().ok_or("index 0 in range")?, [0x30, 0x07, 0xD6]);
    assert_eq!(*out.get(1).ok_or("index 1 in range")?, [0x3C, 0x03, 0xDC]);
    assert_eq!(*out.get(2).ok_or("index 2 in range")?, [0x31, 0x6F, 0xC4]);
    assert_eq!(*out.get(3).ok_or("index 3 in range")?, [0x3F, 0x1D, 0xDF]);
    assert_eq!(*out.get(4).ok_or("index 4 in range")?, [0x32, 0x0B, 0xB3]);
    assert_eq!(*out.get(5).ok_or("index 5 in range")?, [0x50, 0x6F, 0xB3]);
    assert_eq!(*out.get(6).ok_or("index 6 in range")?, [0x33, 0x6F, 0xB3]);
    assert_eq!(*out.get(7).ok_or("index 7 in range")?, [0x50, 0x6F, 0xB3]);
    Ok(())
}
