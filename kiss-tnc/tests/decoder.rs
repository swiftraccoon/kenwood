//! Streaming `KissDecoder` reassembly tests.

use proptest as _;
use thiserror as _;

use kiss_tnc::{
    FEND, FESC, KissCommand, KissDecoder, KissError, KissFrame, KissPort, TFEND, TFESC,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Drain every frame currently available from the decoder, collecting
/// each frame's payload. Stops at the first `Ok(None)`; an error aborts
/// the drain and is returned to the caller.
fn drain_payloads(decoder: &mut KissDecoder) -> Result<Vec<Vec<u8>>, KissError> {
    let mut out = Vec::new();
    while let Some(frame) = decoder.next_frame()? {
        out.push(frame.data);
    }
    Ok(out)
}

#[test]
fn yields_single_complete_frame() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[FEND, 0x00, 0xAA, 0xBB, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA, 0xBB]]);
    Ok(())
}

/// The KISS spec (Chepponis/Karn) states a single FEND both ends one
/// frame and begins the next, so `FEND f1 FEND f2 FEND f3 FEND` is a
/// legal three-frame stream. The decoder must surface all three.
#[test]
fn shared_fend_delimiter_yields_every_frame() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[FEND, 0x00, 0xA0, FEND, 0x00, 0xA1, FEND, 0x00, 0xA2, FEND]);
    assert_eq!(
        drain_payloads(&mut d)?,
        vec![vec![0xA0], vec![0xA1], vec![0xA2]],
    );
    Ok(())
}

#[test]
fn double_fend_between_frames_yields_both() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[FEND, 0x00, 0xAA, FEND, FEND, 0x00, 0xBB, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA], vec![0xBB]]);
    Ok(())
}

#[test]
fn partial_frame_completes_after_more_bytes() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[FEND, 0x00, 0xAA]);
    assert!(
        drain_payloads(&mut d)?.is_empty(),
        "the closing FEND has not arrived"
    );
    d.push(&[0xBB, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA, 0xBB]]);
    Ok(())
}

#[test]
fn byte_at_a_time_reassembles() -> TestResult {
    let mut d = KissDecoder::new();
    for b in [FEND, 0x00, 0xAA, 0xBB, FEND] {
        d.push(&[b]);
    }
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA, 0xBB]]);
    Ok(())
}

#[test]
fn discards_leading_garbage_before_first_fend() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[0x11, 0x22, 0x33, FEND, 0x00, 0xAA, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA]]);
    Ok(())
}

#[test]
fn skips_empty_fend_fend_frames() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[FEND, FEND, FEND, 0x00, 0xAA, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA]]);
    Ok(())
}

#[test]
fn destuffs_escaped_bytes_in_stream() -> TestResult {
    let mut d = KissDecoder::new();
    // Payload bytes FEND and FESC arrive escaped as FESC TFEND / FESC TFESC.
    d.push(&[FEND, 0x00, FESC, TFEND, FESC, TFESC, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![FEND, FESC]]);
    Ok(())
}

#[test]
fn propagates_invalid_escape_error() {
    let mut d = KissDecoder::new();
    d.push(&[FEND, 0x00, FESC, 0x99, FEND]);
    let result = d.next_frame();
    assert!(
        matches!(result, Err(KissError::InvalidEscapeSequence)),
        "expected InvalidEscapeSequence, got {result:?}",
    );
}

#[test]
fn rejects_complete_frame_exceeding_cap() {
    let mut d = KissDecoder::with_max_frame_len(8);
    d.push(&[FEND, 0x00]);
    d.push(&[0xAA; 20]);
    d.push(&[FEND]);
    let result = d.next_frame();
    assert!(
        matches!(result, Err(KissError::FrameTooLong)),
        "expected FrameTooLong, got {result:?}",
    );
}

#[test]
fn resyncs_to_next_frame_after_overlong_frame() -> TestResult {
    let mut d = KissDecoder::with_max_frame_len(8);
    d.push(&[FEND, 0x00]);
    d.push(&[0xAA; 20]);
    d.push(&[FEND, 0x00, 0xBB, FEND]);
    let first = d.next_frame();
    assert!(
        matches!(first, Err(KissError::FrameTooLong)),
        "expected FrameTooLong, got {first:?}",
    );
    let second = d.next_frame()?.ok_or("expected a frame after resync")?;
    assert_eq!(second.data, vec![0xBB]);
    Ok(())
}

#[test]
fn rejects_unframed_buffer_exceeding_cap() {
    // Bytes that never contain a FEND must not grow the buffer unbounded.
    let mut d = KissDecoder::with_max_frame_len(8);
    d.push(&[0x11; 20]);
    let result = d.next_frame();
    assert!(
        matches!(result, Err(KissError::FrameTooLong)),
        "expected FrameTooLong, got {result:?}",
    );
}

#[test]
fn default_cap_accepts_a_normal_frame() -> TestResult {
    let mut d = KissDecoder::new();
    d.push(&[FEND, 0x00, 0xAA, 0xBB, FEND]);
    assert_eq!(drain_payloads(&mut d)?, vec![vec![0xAA, 0xBB]]);
    Ok(())
}

#[test]
fn wire_cap_counts_stuffed_type_payload_and_both_delimiters() -> TestResult {
    let frame = KissFrame {
        port: KissPort::try_from(12_u8)?,
        command: KissCommand::Data,
        data: vec![FESC],
    };
    let wire = frame.to_bytes();
    assert_eq!(
        wire.len(),
        6,
        "type and payload each require two stuffed bytes"
    );

    let mut exact = KissDecoder::with_max_frame_len(wire.len());
    exact.push(&wire);
    assert_eq!(exact.next_frame()?, Some(frame));
    assert_eq!(exact.next_frame()?, None);

    let mut exceeded = KissDecoder::with_max_frame_len(wire.len() - 1);
    exceeded.push(&wire);
    let result = exceeded.next_frame();
    assert!(
        matches!(result, Err(KissError::FrameTooLong)),
        "one wire byte over the limit must be rejected: {result:?}",
    );
    Ok(())
}

#[test]
fn queued_valid_frames_can_together_exceed_the_frame_cap() -> TestResult {
    let mut decoder = KissDecoder::with_max_frame_len(4);
    decoder.push(&[FEND, 0x00, 0xA0, FEND, 0x00, 0xA1, FEND, 0x00, 0xA2, FEND]);
    assert_eq!(
        drain_payloads(&mut decoder)?,
        vec![vec![0xA0], vec![0xA1], vec![0xA2]],
        "the per-frame cap must preserve every queued valid frame",
    );
    Ok(())
}

#[test]
fn escape_pair_can_cross_an_input_chunk_boundary() -> TestResult {
    let mut decoder = KissDecoder::new();
    decoder.push(&[FEND, 0x00, FESC]);
    assert_eq!(decoder.next_frame()?, None);
    decoder.push(&[TFEND, FEND]);
    assert_eq!(drain_payloads(&mut decoder)?, vec![vec![FEND]]);
    Ok(())
}

#[test]
fn malformed_frame_preserves_the_following_shared_delimiter_frame() -> TestResult {
    let mut decoder = KissDecoder::new();
    decoder.push(&[FEND, 0x00, FESC, 0x99, FEND, 0x00, 0xAA, FEND]);
    let result = decoder.next_frame();
    assert!(
        matches!(result, Err(KissError::InvalidEscapeSequence)),
        "the malformed frame must be rejected: {result:?}",
    );
    assert_eq!(drain_payloads(&mut decoder)?, vec![vec![0xAA]]);
    Ok(())
}

#[test]
fn incomplete_frame_at_cap_waits_and_over_cap_resynchronizes() -> TestResult {
    let mut decoder = KissDecoder::with_max_frame_len(4);
    decoder.push(&[FEND, 0x00, 0xAA, 0xBB]);
    assert_eq!(decoder.next_frame()?, None);
    decoder.push(&[0xCC]);
    let result = decoder.next_frame();
    assert!(
        matches!(result, Err(KissError::FrameTooLong)),
        "the oversized incomplete frame must be discarded: {result:?}",
    );
    assert_eq!(decoder.next_frame()?, None);
    decoder.push(&[0x11, FEND, 0x00, 0xDD, FEND]);
    assert_eq!(drain_payloads(&mut decoder)?, vec![vec![0xDD]]);
    Ok(())
}

#[test]
fn caps_below_the_minimum_reject_complete_frames() {
    for cap in 0..3 {
        let mut decoder = KissDecoder::with_max_frame_len(cap);
        decoder.push(&[FEND, 0x00, FEND]);
        let result = decoder.next_frame();
        assert!(
            matches!(result, Err(KissError::FrameTooLong)),
            "cap {cap} must reject a three-byte frame: {result:?}",
        );
    }
}
