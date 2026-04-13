//! Regression test: end-to-end MMDVM wire round-trip preserves core's
//! `DStarHeader` fields via the `mmdvm-core` codec. Proves that the
//! thd75 → mmdvm-core integration point does not lose information
//! when carrying D-STAR protocol types through the MMDVM binary wire
//! format.

use dstar_gateway_core::{Callsign, DStarHeader, Suffix, VoiceFrame};
use mmdvm_core::{
    MMDVM_DSTAR_DATA, MMDVM_DSTAR_HEADER, MMDVM_FRAME_START, MmdvmFrame, decode_frame, encode_frame,
};

// Acknowledge dev-deps the integration test binary pulls transitively
// so `-D unused-crate-dependencies` doesn't fire.
use kenwood_thd75 as _;
use proptest as _;
use serde_json as _;
use tokio as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn dstar_header_survives_mmdvm_wire_roundtrip() -> TestResult {
    let original = DStarHeader {
        flag1: 0x40,
        flag2: 0x21,
        flag3: 0x03,
        rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"W1AW    "),
        my_suffix: Suffix::from_wire_bytes(*b"ECHO"),
    };

    // Build the wire frame the way the gateway now does: encode the
    // header to 41 bytes, wrap in an `MmdvmFrame` with the D-STAR
    // header command, and run it through `mmdvm-core`'s codec.
    let encoded = original.encode();
    let frame = MmdvmFrame::with_payload(MMDVM_DSTAR_HEADER, encoded.to_vec());
    let wire = encode_frame(&frame)?;
    assert_eq!(wire.len(), 44);
    assert_eq!(wire.first().copied().ok_or("wire[0]")?, MMDVM_FRAME_START);
    assert_eq!(wire.get(1).copied().ok_or("wire[1]")?, 44);
    assert_eq!(wire.get(2).copied().ok_or("wire[2]")?, MMDVM_DSTAR_HEADER);

    let (decoded_frame, consumed) = decode_frame(&wire)?.ok_or("expected full frame")?;
    assert_eq!(consumed, 44);
    assert_eq!(decoded_frame.command, MMDVM_DSTAR_HEADER);

    // Re-decode the 41-byte payload back into a `DStarHeader` and
    // compare field-for-field.
    let payload: [u8; 41] = decoded_frame
        .payload
        .as_slice()
        .try_into()
        .map_err(|_| "expected 41-byte D-STAR header payload")?;
    let decoded = DStarHeader::decode(&payload);
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

    let frame = MmdvmFrame::with_payload(MMDVM_DSTAR_DATA, data.to_vec());
    let wire = encode_frame(&frame)?;
    let (decoded_frame, _) = decode_frame(&wire)?.ok_or("expected full frame")?;
    assert_eq!(decoded_frame.command, MMDVM_DSTAR_DATA);

    let payload = decoded_frame.payload.as_slice();
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
    // Real reflectors occasionally emit non-printable bytes in callsign
    // fields. Core's lenient decode accepts them; the MMDVM wire path
    // must preserve them verbatim too.
    let mut rpt1_bytes = [b' '; 8];
    if let Some(b) = rpt1_bytes.get_mut(0) {
        *b = 0xC3;
    }
    if let Some(b) = rpt1_bytes.get_mut(1) {
        *b = 0xA9;
    }
    let original = DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"REF001 G"),
        rpt1: Callsign::from_wire_bytes(rpt1_bytes),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"N0CALL  "),
        my_suffix: Suffix::from_wire_bytes(*b"    "),
    };

    let encoded = original.encode();
    let frame = MmdvmFrame::with_payload(MMDVM_DSTAR_HEADER, encoded.to_vec());
    let wire = encode_frame(&frame)?;
    let (decoded_frame, _) = decode_frame(&wire)?.ok_or("expected full frame")?;
    let payload: [u8; 41] = decoded_frame
        .payload
        .as_slice()
        .try_into()
        .map_err(|_| "expected 41-byte D-STAR header payload")?;
    let decoded = DStarHeader::decode(&payload);
    assert_eq!(decoded, original);
    Ok(())
}
