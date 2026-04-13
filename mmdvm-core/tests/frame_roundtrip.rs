//! Wire frame encode/decode round-trip tests.

// Integration-test crate sees every workspace dev-dep; acknowledge
// the ones we don't touch directly to satisfy
// `-D unused-crate-dependencies`.
use thiserror as _;
use tracing as _;

use mmdvm_core::{
    MAX_PAYLOAD_LEN, MMDVM_ACK, MMDVM_DMR_DATA1, MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT,
    MMDVM_DSTAR_HEADER, MMDVM_DSTAR_LOST, MMDVM_FRAME_START, MMDVM_GET_STATUS, MMDVM_GET_VERSION,
    MMDVM_NAK, MMDVM_SET_CONFIG, MMDVM_SET_FREQ, MMDVM_SET_MODE, MMDVM_YSF_DATA, MmdvmError,
    MmdvmFrame, decode_frame, encode_frame,
};

// `proptest` is a dev-dependency used by the roundtrip property test
// below; acknowledging it explicitly keeps
// `-D unused_crate_dependencies` happy on this test binary.
use proptest::prelude::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn encode_decode_roundtrip_no_payload() -> TestResult {
    let frame = MmdvmFrame::new(MMDVM_GET_VERSION);
    let wire = encode_frame(&frame)?;
    assert_eq!(wire, [MMDVM_FRAME_START, 3, MMDVM_GET_VERSION]);
    let decoded = decode_frame(&wire)?.ok_or("expected full frame")?;
    let (parsed, consumed) = decoded;
    assert_eq!(consumed, 3);
    assert_eq!(parsed, frame);
    Ok(())
}

#[test]
fn encode_decode_roundtrip_with_payload() -> TestResult {
    let frame = MmdvmFrame::with_payload(MMDVM_DSTAR_DATA, (0..12u8).collect());
    let wire = encode_frame(&frame)?;
    assert_eq!(wire.len(), 15);
    let (parsed, consumed) = decode_frame(&wire)?.ok_or("expected full frame")?;
    assert_eq!(consumed, 15);
    assert_eq!(parsed, frame);
    Ok(())
}

#[test]
fn decode_incomplete_returns_none() -> TestResult {
    // Only start byte.
    assert!(decode_frame(&[MMDVM_FRAME_START])?.is_none());
    // Length claims 5, only 3 bytes present.
    assert!(decode_frame(&[MMDVM_FRAME_START, 5, MMDVM_GET_VERSION])?.is_none());
    // Totally empty.
    assert!(decode_frame(&[])?.is_none());
    Ok(())
}

#[test]
fn decode_invalid_start_byte() {
    let result = decode_frame(&[0xFF, 3, MMDVM_GET_VERSION]);
    assert!(
        matches!(result, Err(MmdvmError::InvalidStartByte { got: 0xFF })),
        "got {result:?}"
    );
}

#[test]
fn decode_invalid_length() {
    let result = decode_frame(&[MMDVM_FRAME_START, 2, MMDVM_GET_VERSION]);
    assert!(
        matches!(result, Err(MmdvmError::InvalidLength { len: 2 })),
        "got {result:?}"
    );
}

#[test]
fn encode_payload_too_large_errors() {
    let frame = MmdvmFrame::with_payload(MMDVM_DSTAR_DATA, vec![0u8; MAX_PAYLOAD_LEN + 1]);
    let result = encode_frame(&frame);
    assert!(
        matches!(
            result,
            Err(MmdvmError::PayloadTooLarge { len }) if len == MAX_PAYLOAD_LEN + 1
        ),
        "got {result:?}"
    );
}

#[test]
fn decode_with_trailing_data_returns_consumed() -> TestResult {
    // Frame of length 3, followed by unrelated trailing bytes.
    let wire = vec![MMDVM_FRAME_START, 3, MMDVM_GET_VERSION, 0xAA, 0xBB];
    let (frame, consumed) = decode_frame(&wire)?.ok_or("expected full frame")?;
    assert_eq!(consumed, 3);
    assert_eq!(frame.command, MMDVM_GET_VERSION);
    assert!(frame.payload.is_empty());
    // Trailing bytes are untouched.
    assert_eq!(wire.get(consumed..), Some(&[0xAA, 0xBB][..]));
    Ok(())
}

#[test]
fn encode_all_mode_command_bytes_roundtrip() -> TestResult {
    // A representative selection across every protocol family.
    let commands: &[u8] = &[
        MMDVM_GET_VERSION,
        MMDVM_GET_STATUS,
        MMDVM_SET_CONFIG,
        MMDVM_SET_MODE,
        MMDVM_SET_FREQ,
        MMDVM_DSTAR_HEADER,
        MMDVM_DSTAR_DATA,
        MMDVM_DSTAR_LOST,
        MMDVM_DSTAR_EOT,
        MMDVM_DMR_DATA1,
        MMDVM_YSF_DATA,
        MMDVM_ACK,
        MMDVM_NAK,
    ];
    for &cmd in commands {
        let frame = MmdvmFrame::with_payload(cmd, vec![cmd; 3]);
        let wire = encode_frame(&frame)?;
        let (parsed, consumed) = decode_frame(&wire)?.ok_or("expected full frame")?;
        assert_eq!(
            consumed,
            wire.len(),
            "consumed mismatch for command 0x{cmd:02X}"
        );
        assert_eq!(parsed, frame, "roundtrip failed for command 0x{cmd:02X}");
    }
    Ok(())
}

proptest! {
    /// Any valid encoded frame decodes back to the original.
    #[test]
    fn prop_roundtrip_any_command_payload(
        command in any::<u8>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD_LEN),
    ) {
        let frame = MmdvmFrame::with_payload(command, payload);
        let wire = encode_frame(&frame).map_err(|e| TestCaseError::fail(format!("encode: {e}")))?;
        let (parsed, consumed) = decode_frame(&wire)
            .map_err(|e| TestCaseError::fail(format!("decode: {e}")))?
            .ok_or_else(|| TestCaseError::fail("expected full frame"))?;
        prop_assert_eq!(consumed, wire.len());
        prop_assert_eq!(parsed, frame);
    }
}
