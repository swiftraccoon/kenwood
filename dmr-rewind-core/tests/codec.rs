// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Protocol golden vectors, round trips, and malformed-input tests.

use dmr_rewind_core::{
    AUTHENTICATION_LEN, Callsign, CodecError, DEFAULT_OPEN_TERMINAL_PORT, DMR_AUDIO_LEN,
    DMR_EMBEDDED_DATA_LEN, FULL_LINK_CONTROL_LEN, FullLinkControl, FullLinkControlType, HEADER_LEN,
    Header, MAX_DATAGRAM_LEN, MAX_PAYLOAD_LEN, Packet, PacketFlags, PacketType, Payload,
    SERVICE_OPEN_TERMINAL, SIGNATURE, SUPER_HEADER_LEN, SessionPoll, SessionType, Subscription,
    SuperHeader, VersionData, authentication_digest, decode, encode,
};
use sha2 as _;
use thiserror as _;

fn raw_datagram(
    packet_type: u16,
    flags: u16,
    sequence: u32,
    declared: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(HEADER_LEN + payload.len());
    datagram.extend_from_slice(&SIGNATURE);
    datagram.extend_from_slice(&packet_type.to_le_bytes());
    datagram.extend_from_slice(&flags.to_le_bytes());
    datagram.extend_from_slice(&sequence.to_le_bytes());
    datagram.extend_from_slice(&declared.to_le_bytes());
    datagram.extend_from_slice(payload);
    datagram
}

fn exact_datagram(packet_type: u16, payload: &[u8]) -> Vec<u8> {
    let declared = u16::try_from(payload.len()).unwrap_or(u16::MAX);
    raw_datagram(packet_type, 0, 0, declared, payload)
}

fn assert_round_trip(packet: &Packet) -> Result<(), CodecError> {
    let datagram = encode(packet)?;
    let decoded = decode(&datagram)?;
    assert_eq!(&decoded, packet, "encoded packet must decode losslessly");
    assert_eq!(
        encode(&decoded)?,
        datagram,
        "decoded packet must re-encode byte-for-byte"
    );
    Ok(())
}

const fn sample_link_control() -> FullLinkControl {
    FullLinkControl {
        flco: 0,
        feature_id: 0,
        service_options: 4,
        destination_id: 0x12_34_56,
        source_id: 0x65_43_21,
        tail: [0xaa, 0xbb, 0xcc],
    }
}

#[test]
fn public_constants_match_the_wire_services() {
    assert_eq!(SIGNATURE, *b"REWIND01", "signature must match REWIND");
    assert_eq!(HEADER_LEN, 18, "envelope must be exactly 18 bytes");
    assert_eq!(
        DEFAULT_OPEN_TERMINAL_PORT, 54_006,
        "Open Terminal must use its self-service port"
    );
    assert_eq!(
        SERVICE_OPEN_TERMINAL, 0x21,
        "Open Terminal service byte must match Rewind.h"
    );
    assert_eq!(
        AUTHENTICATION_LEN, 32,
        "SHA-256 responses must contain 32 bytes"
    );
    assert_eq!(
        FULL_LINK_CONTROL_LEN, 12,
        "full link control must contain 12 bytes"
    );
    assert_eq!(
        DMR_AUDIO_LEN, 27,
        "audio bursts must contain three nine-byte AMBE frames"
    );
    assert_eq!(
        DMR_EMBEDDED_DATA_LEN, 10,
        "embedded data must contain 10 bytes"
    );
    assert_eq!(SUPER_HEADER_LEN, 32, "super-header must contain 32 bytes");
    assert_eq!(
        MAX_PAYLOAD_LEN + HEADER_LEN,
        MAX_DATAGRAM_LEN,
        "payload limit must account for the envelope"
    );
}

#[test]
fn packet_flags_preserve_known_and_future_bits() {
    let flags = PacketFlags::from_bits(0x8005);
    assert!(
        flags.contains(PacketFlags::REAL_TIME_1),
        "real-time bit one must be detectable"
    );
    assert!(
        !flags.contains(PacketFlags::REAL_TIME_2),
        "unset real-time bit two must remain unset"
    );
    assert!(
        flags.contains(PacketFlags::BUFFERING),
        "buffering bit must be detectable"
    );
    assert_eq!(flags.bits(), 0x8005, "unknown flag bits must be preserved");
    assert_eq!(PacketFlags::NONE.bits(), 0, "NONE must encode as zero");
}

#[test]
fn packet_type_mapping_is_canonical_and_lossless() {
    let cases = [
        (0x0000, PacketType::KeepAlive),
        (0x0001, PacketType::Close),
        (0x0002, PacketType::Challenge),
        (0x0003, PacketType::Authentication),
        (0x0008, PacketType::Redirection),
        (0x0100, PacketType::Report),
        (0x0200, PacketType::BusyNotice),
        (0x0201, PacketType::AddressNotice),
        (0x0202, PacketType::BindingNotice),
        (0x0800, PacketType::ExternalServer),
        (0x0801, PacketType::RemoteControl),
        (0x0802, PacketType::SnmpTrap),
        (0x0810, PacketType::PeerData),
        (0x0811, PacketType::RdacData),
        (0x0812, PacketType::MediaData),
        (0x0900, PacketType::Configuration),
        (0x0901, PacketType::Subscription),
        (0x0902, PacketType::Cancelling),
        (0x0903, PacketType::SessionPoll),
        (0x0910, PacketType::DmrData(0)),
        (0x0911, PacketType::DmrVoiceHeader),
        (0x0912, PacketType::DmrTerminator),
        (0x0913, PacketType::DmrData(3)),
        (0x091f, PacketType::DmrData(15)),
        (0x0920, PacketType::DmrAudio(0)),
        (0x0926, PacketType::DmrAudio(6)),
        (0x0927, PacketType::DmrEmbeddedData),
        (0x0928, PacketType::SuperHeader),
        (0x0929, PacketType::Failure),
        (0x0a00, PacketType::TerminalIdle),
        (0x0a02, PacketType::TerminalAttach),
        (0x0a03, PacketType::TerminalDetach),
        (0x0a04, PacketType::TerminalWakeup),
        (0x0a10, PacketType::MessageText),
        (0x0a11, PacketType::MessageStatus),
        (0x0a20, PacketType::LocationReport),
        (0x0a21, PacketType::LocationRequest),
        (0xbeef, PacketType::Unknown(0xbeef)),
    ];

    for (raw, packet_type) in cases {
        assert_eq!(
            PacketType::from_raw(raw),
            packet_type,
            "raw type {raw:#06x} must select its canonical variant"
        );
        assert_eq!(
            packet_type.as_raw(),
            raw,
            "canonical type must preserve raw value {raw:#06x}"
        );
    }
}

#[test]
fn open_terminal_keepalive_matches_golden_vector() -> Result<(), CodecError> {
    let packet = Packet::new(
        PacketType::KeepAlive,
        PacketFlags::NONE,
        0x0102_0304,
        Payload::KeepAlive(Some(VersionData {
            remote_id: 1_234_567,
            service: SERVICE_OPEN_TERMINAL,
            description: b"pulsar/1".to_vec(),
        })),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x00, 0x00, 0x00, 0x00, 0x04, 0x03, 0x02,
        0x01, 0x0d, 0x00, 0x87, 0xd6, 0x12, 0x00, 0x21, 0x70, 0x75, 0x6c, 0x73, 0x61, 0x72, 0x2f,
        0x31,
    ];

    assert_eq!(
        encode(&packet)?,
        golden,
        "Open Terminal keepalive must use the exact little-endian envelope"
    );
    assert_eq!(
        decode(&golden)?,
        packet,
        "golden keepalive must decode to typed version data"
    );
    Ok(())
}

#[test]
fn subscription_matches_golden_vector() -> Result<(), CodecError> {
    let packet = Packet::new(
        PacketType::Subscription,
        PacketFlags::NONE,
        7,
        Payload::Subscription(Some(Subscription {
            session_type: SessionType::GroupVoice,
            target: 91,
        })),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x01, 0x09, 0x00, 0x00, 0x07, 0x00, 0x00,
        0x00, 0x08, 0x00, 0x07, 0x00, 0x00, 0x00, 0x5b, 0x00, 0x00, 0x00,
    ];

    assert_eq!(
        encode(&packet)?,
        golden,
        "subscription words must be little-endian"
    );
    assert_eq!(
        decode(&golden)?,
        packet,
        "golden subscription must decode losslessly"
    );
    Ok(())
}

#[test]
fn full_link_control_matches_golden_vector() -> Result<(), CodecError> {
    let link_control = sample_link_control();
    let raw = [
        0x00, 0x00, 0x04, 0x12, 0x34, 0x56, 0x65, 0x43, 0x21, 0xaa, 0xbb, 0xcc,
    ];
    assert_eq!(
        FullLinkControl::from_bytes(raw),
        link_control,
        "DMR IDs must decode as 24-bit big-endian fields"
    );
    assert_eq!(
        link_control.to_bytes()?,
        raw,
        "full link control must re-encode byte-for-byte"
    );
    assert_eq!(
        link_control.call_type(),
        FullLinkControlType::Group,
        "FLCO zero must classify as group voice"
    );

    let packet = Packet::new(
        PacketType::DmrVoiceHeader,
        PacketFlags::REAL_TIME_1,
        0x1122_3344,
        Payload::DmrVoiceHeader(link_control),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x11, 0x09, 0x01, 0x00, 0x44, 0x33, 0x22,
        0x11, 0x0c, 0x00, 0x00, 0x00, 0x04, 0x12, 0x34, 0x56, 0x65, 0x43, 0x21, 0xaa, 0xbb, 0xcc,
    ];
    assert_eq!(
        encode(&packet)?,
        golden,
        "voice header must carry exactly twelve FLC bytes"
    );
    assert_eq!(
        decode(&golden)?,
        packet,
        "golden voice header must decode losslessly"
    );
    Ok(())
}

#[test]
fn terminator_forms_match_golden_vectors() -> Result<(), CodecError> {
    let empty_packet = Packet::new(
        PacketType::DmrTerminator,
        PacketFlags::REAL_TIME_1,
        5,
        Payload::DmrTerminator(None),
    )?;
    let empty_golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x12, 0x09, 0x01, 0x00, 0x05, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ];
    assert_eq!(
        encode(&empty_packet)?,
        empty_golden,
        "empty terminator must declare a zero-byte payload"
    );
    assert_eq!(
        decode(&empty_golden)?,
        empty_packet,
        "empty terminator must remain distinguishable"
    );

    let linked_packet = Packet::new(
        PacketType::DmrTerminator,
        PacketFlags::REAL_TIME_1,
        6,
        Payload::DmrTerminator(Some(sample_link_control())),
    )?;
    let linked_golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x12, 0x09, 0x01, 0x00, 0x06, 0x00, 0x00,
        0x00, 0x0c, 0x00, 0x00, 0x00, 0x04, 0x12, 0x34, 0x56, 0x65, 0x43, 0x21, 0xaa, 0xbb, 0xcc,
    ];
    assert_eq!(
        encode(&linked_packet)?,
        linked_golden,
        "linked terminator must declare exactly twelve payload bytes"
    );
    assert_eq!(
        decode(&linked_golden)?,
        linked_packet,
        "linked terminator must retain its full link control"
    );
    Ok(())
}

#[test]
fn full_link_control_classifies_and_preserves_flco_values() {
    for (flco, expected) in [
        (0, FullLinkControlType::Group),
        (3, FullLinkControlType::Private),
        (0x80, FullLinkControlType::Group),
        (0xc3, FullLinkControlType::Private),
        (0xbf, FullLinkControlType::Unknown(0x3f)),
        (0xff, FullLinkControlType::Unknown(0x3f)),
    ] {
        let value = FullLinkControl {
            flco,
            ..sample_link_control()
        };
        assert_eq!(
            value.call_type(),
            expected,
            "control octet {flco:#04x} must classify by its low six bits"
        );
        assert_eq!(
            expected.as_flco(),
            flco & 0x3f,
            "classification must return the masked FLCO value"
        );
        assert_eq!(
            value.flco, flco,
            "full link control must preserve protection and reserved bits"
        );
    }
}

#[test]
fn super_header_matches_golden_vector() -> Result<(), CodecError> {
    let metadata = SuperHeader {
        session_type: SessionType::GroupVoice,
        source_id: 1_234_567,
        target_id: 91,
        source_call: Callsign::from_raw(*b"N0CALL\0\0\0\0"),
        target_call: Callsign::from_raw(*b"TG91\0\0\0\0\0\0"),
    };
    let packet = Packet::new(
        PacketType::SuperHeader,
        PacketFlags::REAL_TIME_1,
        9,
        Payload::SuperHeader(metadata),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x28, 0x09, 0x01, 0x00, 0x09, 0x00, 0x00,
        0x00, 0x20, 0x00, 0x07, 0x00, 0x00, 0x00, 0x87, 0xd6, 0x12, 0x00, 0x5b, 0x00, 0x00, 0x00,
        0x4e, 0x30, 0x43, 0x41, 0x4c, 0x4c, 0x00, 0x00, 0x00, 0x00, 0x54, 0x47, 0x39, 0x31, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    assert_eq!(
        encode(&packet)?,
        golden,
        "super-header integers must be little-endian and callsigns exact"
    );
    assert_eq!(
        decode(&golden)?,
        packet,
        "golden super-header must decode losslessly"
    );
    Ok(())
}

#[test]
fn authentication_digest_matches_sha256_vector() {
    let expected = [
        0x65, 0x54, 0x2e, 0x59, 0xe1, 0x21, 0x19, 0x06, 0x05, 0xaa, 0x16, 0xe0, 0xdc, 0xd9, 0xef,
        0x22, 0x4e, 0x1b, 0xf2, 0x6f, 0xbf, 0x16, 0x1a, 0xf0, 0xf9, 0x28, 0x99, 0xfe, 0x99, 0x5a,
        0x3c, 0x7c,
    ];
    assert_eq!(
        authentication_digest(&[0x12, 0x34, 0x56, 0x78], b"secret"),
        expected,
        "digest must be SHA-256 of challenge followed directly by password"
    );
}

#[test]
fn rewind_control_payloads_round_trip() -> Result<(), CodecError> {
    let cases = vec![
        Packet::new(
            PacketType::KeepAlive,
            PacketFlags::NONE,
            1,
            Payload::KeepAlive(None),
        )?,
        Packet::new(
            PacketType::KeepAlive,
            PacketFlags::NONE,
            2,
            Payload::KeepAlive(Some(VersionData {
                remote_id: 42,
                service: SERVICE_OPEN_TERMINAL,
                description: vec![0, 0xff, b'R'],
            })),
        )?,
        Packet::new(PacketType::Close, PacketFlags::NONE, 3, Payload::Close)?,
        Packet::new(
            PacketType::Challenge,
            PacketFlags::NONE,
            4,
            Payload::Challenge(vec![1, 2, 3, 4]),
        )?,
        Packet::new(
            PacketType::Authentication,
            PacketFlags::NONE,
            5,
            Payload::Authentication([0xa5; AUTHENTICATION_LEN]),
        )?,
        Packet::new(
            PacketType::Redirection,
            PacketFlags::NONE,
            6,
            Payload::Redirection(vec![1, 0, 2, 0, 3]),
        )?,
        Packet::new(
            PacketType::Report,
            PacketFlags::NONE,
            7,
            Payload::Report(b"connected".to_vec()),
        )?,
        Packet::new(
            PacketType::BusyNotice,
            PacketFlags::NONE,
            8,
            Payload::Busy(b"busy".to_vec()),
        )?,
    ];

    for packet in cases {
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn open_terminal_control_payloads_round_trip() -> Result<(), CodecError> {
    let cases = vec![
        Packet::new(
            PacketType::Subscription,
            PacketFlags::NONE,
            11,
            Payload::Subscription(None),
        )?,
        Packet::new(
            PacketType::Subscription,
            PacketFlags::NONE,
            12,
            Payload::Subscription(Some(Subscription {
                session_type: SessionType::Unknown(0xfeed_beef),
                target: 0x0102_0304,
            })),
        )?,
        Packet::new(
            PacketType::Cancelling,
            PacketFlags::NONE,
            13,
            Payload::Cancelling(None),
        )?,
        Packet::new(
            PacketType::Cancelling,
            PacketFlags::NONE,
            14,
            Payload::Cancelling(Some(Subscription {
                session_type: SessionType::PrivateVoice,
                target: 3_102_605,
            })),
        )?,
        Packet::new(
            PacketType::SessionPoll,
            PacketFlags::NONE,
            15,
            Payload::SessionPoll(SessionPoll {
                kind: 1,
                flags: 2,
                number: 3,
                state: 4,
            }),
        )?,
    ];

    for packet in cases {
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn modeled_media_payloads_round_trip() -> Result<(), CodecError> {
    let link_control = sample_link_control();
    let cases = vec![
        Packet::new(
            PacketType::DmrVoiceHeader,
            PacketFlags::REAL_TIME_1,
            16,
            Payload::DmrVoiceHeader(link_control),
        )?,
        Packet::new(
            PacketType::DmrTerminator,
            PacketFlags::REAL_TIME_1,
            17,
            Payload::DmrTerminator(Some(FullLinkControl {
                flco: 3,
                ..link_control
            })),
        )?,
        Packet::new(
            PacketType::DmrTerminator,
            PacketFlags::REAL_TIME_1,
            18,
            Payload::DmrTerminator(None),
        )?,
        Packet::new(
            PacketType::DmrEmbeddedData,
            PacketFlags::REAL_TIME_1,
            19,
            Payload::DmrEmbeddedData([0x5a; DMR_EMBEDDED_DATA_LEN]),
        )?,
        Packet::new(
            PacketType::SuperHeader,
            PacketFlags::REAL_TIME_1,
            20,
            Payload::SuperHeader(SuperHeader {
                session_type: SessionType::PrivateVoice,
                source_id: 123,
                target_id: 456,
                source_call: Callsign::from_raw(*b"SOURCE\0\0\0\0"),
                target_call: Callsign::from_raw(*b"TARGET\0\0\0\0"),
            }),
        )?,
        Packet::new(
            PacketType::Failure,
            PacketFlags::NONE,
            21,
            Payload::Failure(vec![0xde, 0xad]),
        )?,
    ];

    for packet in cases {
        assert_round_trip(&packet)?;
    }
    for subtype in 0..=6 {
        let packet = Packet::new(
            PacketType::DmrAudio(subtype),
            PacketFlags::from_bits(0x8001),
            u32::from(subtype),
            Payload::DmrAudio([subtype; DMR_AUDIO_LEN]),
        )?;
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn opaque_packet_classes_and_unknown_types_round_trip() -> Result<(), CodecError> {
    let packet_types = [
        PacketType::Configuration,
        PacketType::AddressNotice,
        PacketType::BindingNotice,
        PacketType::ExternalServer,
        PacketType::RemoteControl,
        PacketType::SnmpTrap,
        PacketType::PeerData,
        PacketType::RdacData,
        PacketType::MediaData,
        PacketType::DmrData(0),
        PacketType::DmrData(3),
        PacketType::DmrData(15),
        PacketType::TerminalIdle,
        PacketType::TerminalAttach,
        PacketType::TerminalDetach,
        PacketType::TerminalWakeup,
        PacketType::MessageText,
        PacketType::MessageStatus,
        PacketType::LocationReport,
        PacketType::LocationRequest,
        PacketType::Unknown(0xbeef),
    ];

    for packet_type in packet_types {
        let packet = Packet::new(
            packet_type,
            PacketFlags::from_bits(0xf000),
            0xffff_ffff,
            Payload::Opaque(vec![0, 1, 0xfe, 0xff]),
        )?;
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn unknown_session_values_and_callsign_bytes_are_lossless() -> Result<(), CodecError> {
    let callsign = Callsign::from_raw([b'N', b'0', 0xff, b' ', b' ', 0, b'X', b'X', b'X', b'X']);
    assert_eq!(
        callsign.into_raw(),
        [b'N', b'0', 0xff, b' ', b' ', 0, b'X', b'X', b'X', b'X'],
        "callsign accessor must preserve all ten bytes"
    );
    assert_eq!(
        callsign.trimmed_lossy(),
        "N0�",
        "display helper must stop at NUL and trim trailing spaces"
    );

    let packet = Packet::new(
        PacketType::SuperHeader,
        PacketFlags::NONE,
        0,
        Payload::SuperHeader(SuperHeader {
            session_type: SessionType::Unknown(0x8765_4321),
            source_id: 0xffff_ffff,
            target_id: 0x8000_0000,
            source_call: callsign,
            target_call: Callsign::from_raw([0xff; 10]),
        }),
    )?;
    assert_round_trip(&packet)
}

#[test]
fn malformed_envelopes_are_rejected_before_payload_parsing() {
    for length in 0..HEADER_LEN {
        let datagram = vec![0; length];
        assert_eq!(
            decode(&datagram),
            Err(CodecError::DatagramTooShort {
                actual: length,
                minimum: HEADER_LEN,
            }),
            "every truncated envelope length must return a bounded error"
        );
    }

    let mut invalid_signature = Vec::from(*b"BROKEN!!");
    invalid_signature.extend_from_slice(&[0; HEADER_LEN - 8]);
    assert_eq!(
        decode(&invalid_signature),
        Err(CodecError::InvalidSignature),
        "a complete header with the wrong signature must be rejected"
    );

    let truncated = raw_datagram(0x0001, 0, 0, 1, &[]);
    assert_eq!(
        decode(&truncated),
        Err(CodecError::TruncatedPayload {
            declared: 1,
            actual: 0,
        }),
        "declared bytes missing from the datagram must be rejected"
    );

    let trailing = raw_datagram(0x0001, 0, 0, 0, &[0xff]);
    assert_eq!(
        decode(&trailing),
        Err(CodecError::TrailingBytes { trailing: 1 }),
        "undeclared trailing bytes must be rejected"
    );

    let oversized = vec![0; MAX_DATAGRAM_LEN + 1];
    assert_eq!(
        decode(&oversized),
        Err(CodecError::DatagramTooLong {
            actual: MAX_DATAGRAM_LEN + 1,
            maximum: MAX_DATAGRAM_LEN,
        }),
        "datagrams beyond the UDP maximum must be rejected"
    );
}

#[test]
fn malformed_fixed_size_payloads_are_rejected() {
    let cases = [
        (0x0001, 1),
        (0x0000, 1),
        (0x0000, 4),
        (0x0003, 31),
        (0x0003, 33),
        (0x0901, 1),
        (0x0901, 7),
        (0x0901, 9),
        (0x0902, 1),
        (0x0902, 7),
        (0x0902, 9),
        (0x0903, 15),
        (0x0903, 17),
        (0x0911, 11),
        (0x0911, 13),
        (0x0912, 1),
        (0x0912, 11),
        (0x0912, 13),
        (0x0920, 26),
        (0x0920, 28),
        (0x0926, 26),
        (0x0926, 28),
        (0x0927, 9),
        (0x0927, 11),
        (0x0928, 31),
        (0x0928, 33),
    ];

    for (packet_type, length) in cases {
        let payload = vec![0; length];
        let datagram = exact_datagram(packet_type, &payload);
        assert!(
            matches!(
                decode(&datagram),
                Err(CodecError::InvalidPayloadLength { .. })
            ),
            "type {packet_type:#06x} must reject malformed payload length {length}"
        );
    }
}

#[test]
fn encoder_rejects_inconsistent_public_values() {
    assert_eq!(
        Packet::new(
            PacketType::Close,
            PacketFlags::NONE,
            0,
            Payload::Challenge(vec![])
        ),
        Err(CodecError::PayloadTypeMismatch {
            packet_type: PacketType::Close,
        }),
        "constructor must reject a mismatched payload variant"
    );

    let header_mismatch = Packet {
        header: Header {
            packet_type: PacketType::Close,
            flags: PacketFlags::NONE,
            sequence: 0,
            payload_len: 1,
        },
        payload: Payload::Close,
    };
    assert_eq!(
        encode(&header_mismatch),
        Err(CodecError::HeaderLengthMismatch {
            declared: 1,
            actual: 0,
        }),
        "encoder must not silently repair a public header length"
    );

    for packet_type in [PacketType::DmrData(16), PacketType::DmrAudio(7)] {
        assert!(
            matches!(
                Packet::new(
                    packet_type,
                    PacketFlags::NONE,
                    0,
                    Payload::Opaque(Vec::new())
                ),
                Err(CodecError::InvalidSubtype { .. })
            ),
            "out-of-range public subtype must be rejected"
        );
    }

    for packet_type in [
        PacketType::DmrData(1),
        PacketType::DmrData(2),
        PacketType::Unknown(0x0000),
        PacketType::Unknown(0x0911),
        PacketType::Unknown(0x0920),
    ] {
        assert!(
            matches!(
                Packet::new(
                    packet_type,
                    PacketFlags::NONE,
                    0,
                    Payload::Opaque(Vec::new())
                ),
                Err(CodecError::NonCanonicalPacketType { .. })
            ),
            "aliases of canonical packet variants must be rejected"
        );
    }
}

#[test]
fn full_link_control_rejects_ids_larger_than_24_bits() {
    let invalid_destination = FullLinkControl {
        destination_id: 0x0100_0000,
        ..sample_link_control()
    };
    assert_eq!(
        invalid_destination.to_bytes(),
        Err(CodecError::FieldOutOfRange {
            field: "full link control destination ID",
            value: 0x0100_0000,
            maximum: 0x00ff_ffff,
        }),
        "destination ID must fit three wire bytes"
    );

    let invalid_source = FullLinkControl {
        source_id: u32::MAX,
        ..sample_link_control()
    };
    assert_eq!(
        invalid_source.to_bytes(),
        Err(CodecError::FieldOutOfRange {
            field: "full link control source ID",
            value: u32::MAX,
            maximum: 0x00ff_ffff,
        }),
        "source ID must fit three wire bytes"
    );
}

#[test]
fn codec_accepts_the_largest_udp_payload_and_rejects_one_more() -> Result<(), CodecError> {
    let maximum = Packet::new(
        PacketType::Unknown(0xbeef),
        PacketFlags::NONE,
        0,
        Payload::Opaque(vec![0x5a; MAX_PAYLOAD_LEN]),
    )?;
    let encoded = encode(&maximum)?;
    assert_eq!(
        encoded.len(),
        MAX_DATAGRAM_LEN,
        "largest legal payload must fill one maximum UDP datagram"
    );
    assert_eq!(
        decode(&encoded)?,
        maximum,
        "largest legal datagram must round-trip"
    );

    assert_eq!(
        Packet::new(
            PacketType::Unknown(0xbeef),
            PacketFlags::NONE,
            0,
            Payload::Opaque(vec![0; MAX_PAYLOAD_LEN + 1])
        ),
        Err(CodecError::PayloadTooLong {
            actual: MAX_PAYLOAD_LEN + 1,
            maximum: MAX_PAYLOAD_LEN,
        }),
        "payload one byte over the UDP limit must be rejected"
    );
    Ok(())
}

#[test]
fn arbitrary_short_inputs_return_errors_without_panicking() {
    for length in 0..=256 {
        let input = vec![0xa5; length];
        assert!(
            decode(&input).is_err(),
            "non-REWIND input of length {length} must return an error"
        );
    }
}
