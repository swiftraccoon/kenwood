//! One-shot KISS codec tests: encode/decode round-trips, the Return
//! command, and every malformed-input error path.

use proptest as _;
use thiserror as _;

use kiss_tnc::{
    CMD_RETURN, FEND, FESC, KissCommand, KissError, KissFrame, KissPort, TFEND, TFESC,
    decode_kiss_frame, encode_kiss_frame, encode_kiss_frame_into,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The seven nibble-encoded (non-Return) KISS commands.
const NIBBLE_COMMANDS: [KissCommand; 7] = [
    KissCommand::Data,
    KissCommand::TxDelay,
    KissCommand::Persistence,
    KissCommand::SlotTime,
    KissCommand::TxTail,
    KissCommand::FullDuplex,
    KissCommand::SetHardware,
];

#[test]
fn roundtrip_preserves_port_command_and_data() -> TestResult {
    // Every valid port (0..=15) paired with every nibble command must
    // survive encode -> decode unchanged, including a payload that
    // exercises FEND/FESC byte stuffing.
    for raw_port in 0..=KissPort::MAX.get() {
        let port = KissPort::new(raw_port).ok_or("port in range")?;
        for command in NIBBLE_COMMANDS {
            let data = vec![0x11, FEND, FESC, 0x22];
            let frame = KissFrame {
                port,
                command,
                data: data.clone(),
            };
            let decoded = decode_kiss_frame(&encode_kiss_frame(&frame))?;
            assert_eq!(decoded.port, port, "port {port:?} command {command:?}");
            assert_eq!(
                decoded.command, command,
                "port {port:?} command {command:?}"
            );
            assert_eq!(decoded.data, data, "port {port:?} command {command:?}");
        }
    }
    Ok(())
}

#[test]
fn port_15_data_frame_does_not_collide_with_return() -> TestResult {
    // Port 15 + a low command nibble must never encode to the 0xFF
    // type byte reserved for the whole-byte Return command.
    let port = KissPort::new(15).ok_or("15 is a valid port")?;
    let frame = KissFrame {
        port,
        command: KissCommand::Data,
        data: vec![0xAB],
    };
    let wire = encode_kiss_frame(&frame);
    assert_ne!(
        wire.get(1).copied(),
        Some(CMD_RETURN),
        "type byte must not be 0xFF",
    );
    let decoded = decode_kiss_frame(&wire)?;
    assert_eq!(decoded.port.get(), 15);
    assert_eq!(decoded.command, KissCommand::Data);
    Ok(())
}

#[test]
fn type_byte_equal_to_fend_is_escaped() -> TestResult {
    // Port 12 + command Data yields type byte 0xC0, which equals FEND.
    // It must be byte-stuffed like any frame content, never emitted raw,
    // or the frame is unparseable / mis-framed.
    let port = KissPort::new(12).ok_or("12 is a valid port")?;
    let frame = KissFrame {
        port,
        command: KissCommand::Data,
        data: vec![0x01],
    };
    let wire = encode_kiss_frame(&frame);
    // No raw FEND may appear between the opening and closing delimiters.
    let interior = wire
        .get(1..wire.len() - 1)
        .ok_or("encoded wire too short")?;
    assert!(
        !interior.contains(&FEND),
        "type byte 0xC0 must be escaped, not raw: {wire:02X?}",
    );
    let decoded = decode_kiss_frame(&wire)?;
    assert_eq!(decoded.port, port);
    assert_eq!(decoded.command, KissCommand::Data);
    assert_eq!(decoded.data, vec![0x01]);
    Ok(())
}

#[test]
fn return_frame_encodes_as_three_bare_bytes() -> TestResult {
    // A Return frame is the whole-byte 0xFF command: FEND 0xFF FEND.
    let wire = encode_kiss_frame(&KissFrame::return_command());
    assert_eq!(wire, vec![FEND, CMD_RETURN, FEND]);
    let decoded = decode_kiss_frame(&wire)?;
    assert_eq!(decoded.command, KissCommand::Return);
    assert!(decoded.data.is_empty());
    Ok(())
}

#[test]
fn return_frame_ignores_port_and_data_on_the_wire() -> TestResult {
    // Even built with a non-zero port and a payload, a Return frame's
    // wire form is the canonical bare 0xFF frame (Return carries neither).
    let frame = KissFrame {
        port: KissPort::new(7).ok_or("7 is a valid port")?,
        command: KissCommand::Return,
        data: vec![0xDE, 0xAD],
    };
    assert_eq!(encode_kiss_frame(&frame), vec![FEND, CMD_RETURN, FEND]);
    Ok(())
}

#[test]
fn data_constructor_builds_a_port0_data_frame() -> TestResult {
    let frame = KissFrame::data(vec![1, 2, 3]);
    assert_eq!(frame.port, KissPort::TH_D75);
    assert_eq!(frame.command, KissCommand::Data);
    let decoded = decode_kiss_frame(&encode_kiss_frame(&frame))?;
    assert_eq!(decoded.data, vec![1, 2, 3]);
    Ok(())
}

#[test]
fn encode_into_appends_without_clobbering_existing_bytes() {
    let mut buf = vec![0xEE, 0xEE];
    encode_kiss_frame_into(&KissFrame::data(vec![0xAA]), &mut buf);
    assert_eq!(buf, vec![0xEE, 0xEE, FEND, 0x00, 0xAA, FEND]);
}

#[test]
fn encode_stuffs_special_payload_bytes() {
    // FEND -> FESC TFEND, FESC -> FESC TFESC.
    let wire = encode_kiss_frame(&KissFrame::data(vec![FEND, FESC]));
    assert_eq!(wire, vec![FEND, 0x00, FESC, TFEND, FESC, TFESC, FEND]);
}

#[test]
fn decode_accepts_minimum_length_empty_payload_frame() -> TestResult {
    let frame = decode_kiss_frame(&[FEND, 0x00, FEND])?;
    assert_eq!(frame.command, KissCommand::Data);
    assert!(frame.data.is_empty());
    Ok(())
}

#[test]
fn decode_rejects_unknown_command_nibble() {
    // Low nibble 0x07 is not an assigned KISS command.
    let result = decode_kiss_frame(&[FEND, 0x07, FEND]);
    assert!(
        matches!(result, Err(KissError::UnknownCommand(0x07))),
        "expected UnknownCommand(7), got {result:?}",
    );
}

#[test]
fn decode_rejects_frame_shorter_than_two_bytes() {
    let result = decode_kiss_frame(&[FEND]);
    assert!(
        matches!(result, Err(KissError::FrameTooShort)),
        "got {result:?}",
    );
}

#[test]
fn decode_rejects_missing_start_delimiter() {
    let result = decode_kiss_frame(&[0x00, 0xAA, FEND]);
    assert!(
        matches!(result, Err(KissError::MissingStartDelimiter)),
        "got {result:?}",
    );
}

#[test]
fn decode_rejects_missing_end_delimiter() {
    let result = decode_kiss_frame(&[FEND, 0x00, 0xAA]);
    assert!(
        matches!(result, Err(KissError::MissingEndDelimiter)),
        "got {result:?}",
    );
}

#[test]
fn decode_classifies_bare_delimiters_as_empty_frame() {
    // `FEND FEND` has both delimiters but no type byte.
    let result = decode_kiss_frame(&[FEND, FEND]);
    assert!(
        matches!(result, Err(KissError::EmptyFrame)),
        "got {result:?}",
    );
}

#[test]
fn decode_rejects_invalid_escape_sequence() {
    // FESC must be followed by TFEND or TFESC; 0x99 is neither.
    let result = decode_kiss_frame(&[FEND, 0x00, FESC, 0x99, FEND]);
    assert!(
        matches!(result, Err(KissError::InvalidEscapeSequence)),
        "got {result:?}",
    );
}

#[test]
fn decode_rejects_truncated_escape_sequence() {
    // FESC as the final body byte: nothing follows it.
    let result = decode_kiss_frame(&[FEND, 0x00, FESC, FEND]);
    assert!(
        matches!(result, Err(KissError::TruncatedEscapeSequence)),
        "got {result:?}",
    );
}

#[test]
fn decode_rejects_raw_fend_inside_body() {
    // A raw, unescaped FEND in the payload region is malformed.
    let result = decode_kiss_frame(&[FEND, 0x00, 0xAA, FEND, 0xBB, FEND]);
    assert!(
        matches!(result, Err(KissError::UnexpectedFrameDelimiter)),
        "got {result:?}",
    );
}
