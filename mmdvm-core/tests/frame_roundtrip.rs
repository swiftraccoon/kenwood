//! Framing vectors and incremental decoding properties.

use thiserror as _;

use mmdvm_core::{Command, MAX_PAYLOAD_LEN, MMDVM_FRAME_START, MmdvmError, MmdvmFrame};
use proptest::prelude::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn short_and_extended_wire_vectors_have_independent_expected_headers() -> TestResult {
    assert_eq!(
        MmdvmFrame::new(Command::GET_VERSION).to_bytes(),
        [0xE0, 3, 0]
    );
    let frame = MmdvmFrame::with_payload(Command::DSTAR_DATA, vec![0xAA; 12])?;
    let wire = frame.to_bytes();
    assert_eq!(wire.get(..3), Some([0xE0, 15, 0x11].as_slice()));
    assert_eq!(wire.get(3..), Some([0xAA; 12].as_slice()));

    let frame = MmdvmFrame::with_payload(Command::FM_DATA, vec![0x55; 300])?;
    let wire = frame.to_bytes();
    assert_eq!(wire.get(..4), Some([0xE0, 0, 49, 0x65].as_slice()));
    assert_eq!(wire.len(), 304);
    let (decoded, consumed) = MmdvmFrame::from_bytes(&wire)?.ok_or("missing complete frame")?;
    assert_eq!(decoded, frame);
    assert_eq!(consumed, 304);
    Ok(())
}

#[test]
fn complete_frames_leave_subsequent_bytes_with_the_caller() -> TestResult {
    let wire = [MMDVM_FRAME_START, 3, 0, 0xAA, 0xBB];
    let (frame, consumed) = MmdvmFrame::from_bytes(&wire)?.ok_or("missing complete frame")?;
    assert_eq!(frame.command(), Command::GET_VERSION);
    assert_eq!(frame.payload(), []);
    assert_eq!(consumed, 3);
    assert_eq!(wire.get(consumed..), Some([0xAA, 0xBB].as_slice()));
    Ok(())
}

#[test]
fn malformed_start_and_short_lengths_are_rejected() -> TestResult {
    let _valid = MmdvmFrame::from_bytes(&[MMDVM_FRAME_START, 3, 0])?;
    let invalid = MmdvmFrame::from_bytes(&[0xFF, 3, 0]);
    assert!(
        matches!(invalid, Err(MmdvmError::InvalidStartByte { got: 0xFF })),
        "{invalid:?}"
    );
    for length in [1, 2] {
        let invalid = MmdvmFrame::from_bytes(&[MMDVM_FRAME_START, length]);
        assert!(
            matches!(invalid, Err(MmdvmError::InvalidLength { len }) if len == length),
            "{invalid:?}"
        );
    }
    Ok(())
}

proptest! {
    /// Every encodable command and payload preserves its semantic contents.
    #[test]
    fn prop_roundtrip_any_command_payload(
        command in any::<u8>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD_LEN),
    ) {
        let frame = MmdvmFrame::with_payload(Command::from(command), payload)
            .map_err(to_test_error)?;
        let wire = frame.to_bytes();
        let (parsed, consumed) = MmdvmFrame::from_bytes(&wire)
            .map_err(to_test_error)?
            .ok_or_else(|| TestCaseError::fail("expected full frame"))?;
        prop_assert_eq!(consumed, wire.len());
        prop_assert_eq!(parsed, frame);
    }

    /// Arbitrary input can only produce a bounded frame, an error or an incomplete result.
    #[test]
    fn prop_decode_arbitrary_bytes_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..600),
    ) {
        if let Ok(Some((frame, consumed))) = MmdvmFrame::from_bytes(&data) {
            prop_assert!(consumed <= data.len());
            prop_assert!(frame.payload().len() <= MAX_PAYLOAD_LEN);
        }
    }

    /// Every strict prefix requests more bytes in both wire forms.
    #[test]
    fn prop_partial_frames_request_more_bytes(
        command in any::<u8>(),
        payload in proptest::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD_LEN),
    ) {
        let frame = MmdvmFrame::with_payload(Command::from(command), payload)
            .map_err(to_test_error)?;
        let wire = frame.to_bytes();
        for length in 0..wire.len() {
            let prefix = wire.get(..length).ok_or_else(|| TestCaseError::fail("prefix in bounds"))?;
            let result = MmdvmFrame::from_bytes(prefix);
            prop_assert!(matches!(result, Ok(None)), "prefix {length}: {result:?}");
        }
    }
}

fn to_test_error<E: std::fmt::Debug>(error: E) -> TestCaseError {
    TestCaseError::fail(format!("{error:?}"))
}
