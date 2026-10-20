//! Regression test: end-to-end MMDVM wire round-trip preserves core's
//! `DstarHeader` fields via the `mmdvm-core` codec. Proves that the
//! thd75 → mmdvm-core integration point does not lose information
//! when carrying D-STAR protocol types through the MMDVM binary wire
//! format.

use dstar_gateway_core::{Callsign, DstarHeader, HeaderFlags, Suffix, VoiceFrame};
use mmdvm_core::{Command, MMDVM_FRAME_START, MmdvmFrame};

// Acknowledge dev-deps the integration test binary pulls transitively
// so `-D unused-crate-dependencies` doesn't fire.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use encoding_rs as _;
use kenwood_schema as _;
use kenwood_thd75 as _;
use kenwood_transport as _;
use kiss_tnc as _;
use mmdvm as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn dstar_header_survives_mmdvm_wire_roundtrip() -> TestResult {
    let original = DstarHeader {
        flags: HeaderFlags::from_bytes([0x40, 0x21, 0x03]),
        rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"W1AW    "),
        my_suffix: Suffix::from_wire_bytes(*b"ECHO"),
    };

    // Build the wire frame used by the gateway: encode the
    // header to 41 bytes, wrap in an `MmdvmFrame` with the D-STAR
    // header command, and run it through `mmdvm-core`'s codec.
    let encoded = original.encode();
    let frame = MmdvmFrame::with_payload(Command::DSTAR_HEADER, encoded.to_vec())?;
    let wire = frame.to_bytes();
    assert_eq!(wire.len(), 44);
    assert_eq!(wire.first().copied().ok_or("wire[0]")?, MMDVM_FRAME_START);
    assert_eq!(wire.get(1).copied().ok_or("wire[1]")?, 44);
    assert_eq!(
        wire.get(2).copied().ok_or("wire[2]")?,
        Command::DSTAR_HEADER.get()
    );

    let (decoded_frame, consumed) = MmdvmFrame::from_bytes(&wire)?.ok_or("expected full frame")?;
    assert_eq!(consumed, 44);
    assert_eq!(decoded_frame.command(), Command::DSTAR_HEADER);

    // Re-decode the 41-byte payload back into a `DstarHeader` and
    // compare field-for-field.
    let payload: [u8; 41] = decoded_frame
        .payload()
        .try_into()
        .map_err(|_| "expected 41-byte D-STAR header payload")?;
    let decoded = DstarHeader::decode(&payload);
    assert_eq!(decoded, original);
    Ok(())
}

#[test]
fn voice_frame_survives_mmdvm_wire_roundtrip() -> TestResult {
    let original = VoiceFrame {
        ambe: [0x9A, 0x1B, 0x2C, 0x3D, 0x4E, 0x5F, 0x60, 0x71, 0x82],
        slow_data: [0x30, 0x07, 0xFA],
    };

    let mut data = [0u8; 12];
    data.get_mut(..9)
        .ok_or("data[..9] in range")?
        .copy_from_slice(&original.ambe);
    data.get_mut(9..12)
        .ok_or("data[9..12] in range")?
        .copy_from_slice(&original.slow_data);

    let frame = MmdvmFrame::with_payload(Command::DSTAR_DATA, data.to_vec())?;
    let wire = frame.to_bytes();
    let (decoded_frame, _) = MmdvmFrame::from_bytes(&wire)?.ok_or("expected full frame")?;
    assert_eq!(decoded_frame.command(), Command::DSTAR_DATA);

    let payload = decoded_frame.payload();
    let mut ambe = [0u8; 9];
    ambe.copy_from_slice(payload.get(..9).ok_or("payload[..9] missing")?);
    let mut slow_data = [0u8; 3];
    slow_data.copy_from_slice(payload.get(9..12).ok_or("payload[9..12] missing")?);
    assert_eq!(ambe, original.ambe);
    assert_eq!(slow_data, original.slow_data);
    Ok(())
}

#[test]
fn non_ascii_callsign_bytes_preserved_verbatim() -> TestResult {
    // The lenient header decoder preserves arbitrary callsign bytes; the
    // MMDVM wire path retains the same bytes.
    let mut rpt1_bytes = [b' '; 8];
    if let Some(b) = rpt1_bytes.get_mut(0) {
        *b = 0xC3;
    }
    if let Some(b) = rpt1_bytes.get_mut(1) {
        *b = 0xA9;
    }
    let original = DstarHeader {
        flags: HeaderFlags::NONE,
        rpt2: Callsign::from_wire_bytes(*b"REF001 G"),
        rpt1: Callsign::from_wire_bytes(rpt1_bytes),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"N0CALL  "),
        my_suffix: Suffix::from_wire_bytes(*b"    "),
    };

    let encoded = original.encode();
    let frame = MmdvmFrame::with_payload(Command::DSTAR_HEADER, encoded.to_vec())?;
    let wire = frame.to_bytes();
    let (decoded_frame, _) = MmdvmFrame::from_bytes(&wire)?.ok_or("expected full frame")?;
    let payload: [u8; 41] = decoded_frame
        .payload()
        .try_into()
        .map_err(|_| "expected 41-byte D-STAR header payload")?;
    let decoded = DstarHeader::decode(&payload);
    assert_eq!(decoded, original);
    Ok(())
}
