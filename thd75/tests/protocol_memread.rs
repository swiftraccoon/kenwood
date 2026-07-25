//! Wire-format tests for the memory-read command.
//!
//! The request grammar is fixed by the radio's handler: exactly 13 bytes, with
//! a space at index 2 and a comma at index 9. These tests pin that shape so a
//! serializer change cannot silently produce a request the radio refuses.

// Dependencies visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without weakening
// the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::protocol::memread::encode_hex_upper;
use kenwood_thd75::protocol::{Command, Response, parse, serialize};
use kenwood_thd75::types::{DdrOffset, MEM_READ_BOUND, ReadLen};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Converts a library error into a proptest failure. `unwrap` is banned and
/// `TestCaseError` has no `From` impls for this crate's error types.
fn to_test_err<E: std::fmt::Debug>(e: E) -> TestCaseError {
    TestCaseError::fail(format!("{e:?}"))
}

#[test]
fn serializes_exactly_thirteen_bytes() -> TestResult {
    let cmd = Command::ReadMemory {
        offset: DdrOffset::new(0x17_D1BC)?,
        len: ReadLen::new(64)?,
    };
    let wire = serialize(&cmd);
    assert_eq!(wire, b"GM 17D1BC,40\r".to_vec());
    assert_eq!(wire.len(), 13, "the radio requires exactly 13 bytes");
    Ok(())
}

#[test]
fn pads_offset_to_six_digits_and_len_to_two() -> TestResult {
    let cmd = Command::ReadMemory {
        offset: DdrOffset::ZERO,
        len: ReadLen::new(1)?,
    };
    assert_eq!(serialize(&cmd), b"GM 000000,01\r".to_vec());
    Ok(())
}

#[test]
fn encodes_256_as_double_zero() -> TestResult {
    let cmd = Command::ReadMemory {
        offset: DdrOffset::new(0xFF_FF00)?,
        len: ReadLen::MAX,
    };
    assert_eq!(serialize(&cmd), b"GM FFFF00,00\r".to_vec());
    Ok(())
}

#[test]
fn grammar_anchors_land_where_the_radio_checks_them() -> TestResult {
    let cmd = Command::ReadMemory {
        offset: DdrOffset::new(0xAB_CDEF)?,
        len: ReadLen::new(16)?,
    };
    let wire = serialize(&cmd);
    // The handler rejects the request unless byte[2] is ' ' and byte[9] is ','.
    assert_eq!(wire.get(2).copied().ok_or("wire too short")?, b' ');
    assert_eq!(wire.get(9).copied().ok_or("wire too short")?, b',');
    Ok(())
}

#[test]
fn hex_digits_are_uppercase() -> TestResult {
    let cmd = Command::ReadMemory {
        offset: DdrOffset::new(0xAB_CDEF)?,
        len: ReadLen::new(0xBC)?,
    };
    assert_eq!(serialize(&cmd), b"GM ABCDEF,BC\r".to_vec());
    Ok(())
}

// ---------------------------------------------------------------------------
// Reply parsing
// ---------------------------------------------------------------------------

#[test]
fn parses_reply_with_echoed_offset() -> TestResult {
    let response = parse(b"GM 17D1BC,424D76FA")?;
    assert!(
        matches!(
            &response,
            Response::MemoryData { offset, bytes }
                if offset.as_u32() == 0x17_D1BC
                    && bytes.as_slice() == [0x42, 0x4D, 0x76, 0xFA]
        ),
        "unexpected response: {response:?}"
    );
    Ok(())
}

#[test]
fn parses_lowercase_hex_defensively() -> TestResult {
    // The radio emits uppercase only, but accepting both costs nothing and
    // guards against a relay or future firmware that normalises case.
    let response = parse(b"GM 000010,deadbeef")?;
    assert!(
        matches!(
            &response,
            Response::MemoryData { bytes, .. }
                if bytes.as_slice() == [0xDE, 0xAD, 0xBE, 0xEF]
        ),
        "unexpected response: {response:?}"
    );
    Ok(())
}

#[test]
fn parses_a_full_256_byte_reply() -> TestResult {
    let all_bytes: Vec<u8> = (0u8..=255).collect();
    let frame = format!("GM 000000,{}", encode_hex_upper(&all_bytes));
    let response = parse(frame.as_bytes())?;
    assert!(
        matches!(&response, Response::MemoryData { bytes, .. } if bytes.len() == 256),
        "expected 256 bytes, got {response:?}"
    );
    Ok(())
}

#[test]
fn rejects_odd_length_hex() {
    let result = parse(b"GM 000010,ABC");
    assert!(result.is_err(), "odd hex length must fail, got {result:?}");
}

#[test]
fn rejects_non_hex_payload() {
    let result = parse(b"GM 000010,42ZZ");
    assert!(result.is_err(), "non-hex must fail, got {result:?}");
}

#[test]
fn rejects_missing_comma() {
    let result = parse(b"GM 00001042");
    assert!(result.is_err(), "missing comma must fail, got {result:?}");
}

#[test]
fn rejects_empty_data() {
    // A read is always at least one byte, so no data means a malformed reply.
    let result = parse(b"GM 000010,");
    assert!(result.is_err(), "empty data must fail, got {result:?}");
}

#[test]
fn rejects_offset_above_the_window() {
    let result = parse(b"GM 1000000,42");
    assert!(
        result.is_err(),
        "offset at the bound must fail, got {result:?}"
    );
}

proptest! {
    /// Builds a reply exactly as the radio would, then parses it back.
    #[test]
    fn hex_reply_round_trips(
        offset in 0u32..MEM_READ_BOUND,
        data in proptest::collection::vec(any::<u8>(), 1..=256),
    ) {
        let frame = format!("GM {offset:06X},{}", encode_hex_upper(&data));
        let response = parse(frame.as_bytes()).map_err(to_test_err)?;
        match response {
            Response::MemoryData { offset: got, bytes } => {
                prop_assert_eq!(got.as_u32(), offset);
                prop_assert_eq!(bytes, data);
            }
            other => prop_assert!(false, "wrong variant: {:?}", other),
        }
    }

    /// Every request the planner emits is exactly 13 bytes with the anchors
    /// the radio checks in the positions it checks them.
    #[test]
    fn every_serialized_request_is_well_formed(
        offset in 0u32..MEM_READ_BOUND,
        len in 1u16..=256,
    ) {
        let cmd = Command::ReadMemory {
            offset: DdrOffset::new(offset).map_err(to_test_err)?,
            len: ReadLen::new(len).map_err(to_test_err)?,
        };
        let wire = serialize(&cmd);
        prop_assert_eq!(wire.len(), 13);
        prop_assert_eq!(wire.get(2).copied(), Some(b' '));
        prop_assert_eq!(wire.get(9).copied(), Some(b','));
        prop_assert_eq!(wire.last().copied(), Some(b'\r'));
    }
}
