// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Protocol golden vectors, round trips, and malformed-input tests.

use dmr_rewind_core::{
    AUTHENTICATION_LEN, Callsign, CodecError, ControlOctet, DEFAULT_OPEN_TERMINAL_PORT,
    DMR_AUDIO_LEN, DmrAudioSubtype, DmrDataSubtype, DmrId, FULL_LINK_CONTROL_LEN, FeatureId,
    FullLinkControl, FullLinkControlType, HEADER_LEN, MAX_DATAGRAM_LEN, MAX_PAYLOAD_LEN, Packet,
    PacketFlags, PacketType, Payload, PollFlags, PollKind, PollState, RemoteId,
    SERVICE_OPEN_TERMINAL, SIGNATURE, SUPER_HEADER_LEN, ServiceOptions, SessionPoll, SessionType,
    Subscription, SuperHeader, TransportSequence, UnknownFullLinkControlType, UnknownPacketType,
    UnknownSessionType, VersionData, WireDmrId, authentication_digest,
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
    let datagram = packet.to_bytes();
    let decoded = Packet::from_bytes(&datagram)?;
    assert_eq!(&decoded, packet, "encoded packet must decode losslessly");
    assert_eq!(
        decoded.to_bytes(),
        datagram,
        "decoded packet must re-encode byte-for-byte"
    );
    Ok(())
}

const fn sample_link_control() -> FullLinkControl {
    FullLinkControl {
        flco: ControlOctet::new(0),
        feature_id: FeatureId::new(0),
        service_options: ServiceOptions::from_bits(4),
        destination_id: match DmrId::new(0x12_34_56) {
            Ok(value) => value,
            Err(_) => unreachable!(),
        },
        source_id: match DmrId::new(0x65_43_21) {
            Ok(value) => value,
            Err(_) => unreachable!(),
        },
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
        SERVICE_OPEN_TERMINAL.get(),
        0x21,
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
    let mut masked = flags & !PacketFlags::BUFFERING;
    masked |= PacketFlags::REAL_TIME_2;
    masked ^= PacketFlags::REAL_TIME_1;
    masked &= PacketFlags::from_bits(0x8003);
    assert_eq!(
        masked.bits(),
        0x8002,
        "masking must retain unrelated future bits"
    );
    assert_eq!(format!("{masked:#06x}"), "0x8002");
    assert_eq!(format!("{masked:X}"), "8002");
    assert_eq!(format!("{masked:b}"), "1000000000000010");
    assert_eq!(format!("{masked:o}"), "100002");
}

#[test]
fn packet_type_mapping_is_canonical_and_lossless() -> Result<(), Box<dyn std::error::Error>> {
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
        (0x0910, PacketType::DmrData(DmrDataSubtype::new(0)?)),
        (0x0911, PacketType::DmrVoiceHeader),
        (0x0912, PacketType::DmrTerminator),
        (0x0913, PacketType::DmrData(DmrDataSubtype::new(3)?)),
        (0x091f, PacketType::DmrData(DmrDataSubtype::new(15)?)),
        (0x0920, PacketType::DmrAudio(DmrAudioSubtype::new(0)?)),
        (0x0926, PacketType::DmrAudio(DmrAudioSubtype::new(6)?)),
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
        (0xbeef, PacketType::Unknown(UnknownPacketType::new(0xbeef)?)),
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
    Ok(())
}

#[test]
fn open_terminal_keepalive_matches_golden_vector() -> Result<(), Box<dyn std::error::Error>> {
    let packet = Packet::new(
        PacketType::KeepAlive,
        PacketFlags::NONE,
        TransportSequence::new(0x0102_0304),
        Payload::KeepAlive(Some(VersionData {
            remote_id: RemoteId::new(1_234_567),
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
        packet.to_bytes(),
        golden,
        "Open Terminal keepalive must use the exact little-endian envelope"
    );
    assert_eq!(
        Packet::from_bytes(&golden)?,
        packet,
        "golden keepalive must decode to typed version data"
    );
    Ok(())
}

#[test]
fn subscription_matches_golden_vector() -> Result<(), Box<dyn std::error::Error>> {
    let packet = Packet::new(
        PacketType::Subscription,
        PacketFlags::NONE,
        TransportSequence::new(7),
        Payload::Subscription(Some(Subscription {
            session_type: SessionType::GroupVoice,
            target: WireDmrId::new(91),
        })),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x01, 0x09, 0x00, 0x00, 0x07, 0x00, 0x00,
        0x00, 0x08, 0x00, 0x07, 0x00, 0x00, 0x00, 0x5b, 0x00, 0x00, 0x00,
    ];

    assert_eq!(
        packet.to_bytes(),
        golden,
        "subscription words must be little-endian"
    );
    assert_eq!(
        Packet::from_bytes(&golden)?,
        packet,
        "golden subscription must decode losslessly"
    );
    Ok(())
}

#[test]
fn full_link_control_matches_golden_vector() -> Result<(), Box<dyn std::error::Error>> {
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
        link_control.to_bytes(),
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
        TransportSequence::new(0x1122_3344),
        Payload::DmrVoiceHeader(link_control),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x11, 0x09, 0x01, 0x00, 0x44, 0x33, 0x22,
        0x11, 0x0c, 0x00, 0x00, 0x00, 0x04, 0x12, 0x34, 0x56, 0x65, 0x43, 0x21, 0xaa, 0xbb, 0xcc,
    ];
    assert_eq!(
        packet.to_bytes(),
        golden,
        "voice header must carry exactly twelve FLC bytes"
    );
    assert_eq!(
        Packet::from_bytes(&golden)?,
        packet,
        "golden voice header must decode losslessly"
    );
    Ok(())
}

#[test]
fn terminator_forms_match_golden_vectors() -> Result<(), Box<dyn std::error::Error>> {
    let empty_packet = Packet::new(
        PacketType::DmrTerminator,
        PacketFlags::REAL_TIME_1,
        TransportSequence::new(5),
        Payload::DmrTerminator(None),
    )?;
    let empty_golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x12, 0x09, 0x01, 0x00, 0x05, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ];
    assert_eq!(
        empty_packet.to_bytes(),
        empty_golden,
        "empty terminator must declare a zero-byte payload"
    );
    assert_eq!(
        Packet::from_bytes(&empty_golden)?,
        empty_packet,
        "empty terminator must remain distinguishable"
    );

    let linked_packet = Packet::new(
        PacketType::DmrTerminator,
        PacketFlags::REAL_TIME_1,
        TransportSequence::new(6),
        Payload::DmrTerminator(Some(sample_link_control())),
    )?;
    let linked_golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x12, 0x09, 0x01, 0x00, 0x06, 0x00, 0x00,
        0x00, 0x0c, 0x00, 0x00, 0x00, 0x04, 0x12, 0x34, 0x56, 0x65, 0x43, 0x21, 0xaa, 0xbb, 0xcc,
    ];
    assert_eq!(
        linked_packet.to_bytes(),
        linked_golden,
        "linked terminator must declare exactly twelve payload bytes"
    );
    assert_eq!(
        Packet::from_bytes(&linked_golden)?,
        linked_packet,
        "linked terminator must retain its full link control"
    );
    Ok(())
}

#[test]
fn full_link_control_classifies_and_preserves_flco_values() -> Result<(), Box<dyn std::error::Error>>
{
    for (flco, expected) in [
        (0, FullLinkControlType::Group),
        (3, FullLinkControlType::Private),
        (0x80, FullLinkControlType::Group),
        (0xc3, FullLinkControlType::Private),
        (
            0xbf,
            FullLinkControlType::Unknown(UnknownFullLinkControlType::new(0x3f)?),
        ),
        (
            0xff,
            FullLinkControlType::Unknown(UnknownFullLinkControlType::new(0x3f)?),
        ),
    ] {
        let value = FullLinkControl {
            flco: ControlOctet::new(flco),
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
            value.flco.get(),
            flco,
            "full link control must preserve protection and reserved bits"
        );
    }
    Ok(())
}

#[test]
fn super_header_matches_golden_vector() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = SuperHeader {
        session_type: SessionType::GroupVoice,
        source_id: WireDmrId::new(1_234_567),
        target_id: WireDmrId::new(91),
        source_call: Callsign::from_bytes(*b"N0CALL\0\0\0\0"),
        target_call: Callsign::from_bytes(*b"TG91\0\0\0\0\0\0"),
    };
    let packet = Packet::new(
        PacketType::SuperHeader,
        PacketFlags::REAL_TIME_1,
        TransportSequence::new(9),
        Payload::SuperHeader(metadata),
    )?;
    let golden = vec![
        0x52, 0x45, 0x57, 0x49, 0x4e, 0x44, 0x30, 0x31, 0x28, 0x09, 0x01, 0x00, 0x09, 0x00, 0x00,
        0x00, 0x20, 0x00, 0x07, 0x00, 0x00, 0x00, 0x87, 0xd6, 0x12, 0x00, 0x5b, 0x00, 0x00, 0x00,
        0x4e, 0x30, 0x43, 0x41, 0x4c, 0x4c, 0x00, 0x00, 0x00, 0x00, 0x54, 0x47, 0x39, 0x31, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    assert_eq!(
        packet.to_bytes(),
        golden,
        "super-header integers must be little-endian and callsigns exact"
    );
    assert_eq!(
        Packet::from_bytes(&golden)?,
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
fn rewind_control_payloads_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let cases = vec![
        Packet::new(
            PacketType::KeepAlive,
            PacketFlags::NONE,
            TransportSequence::new(1),
            Payload::KeepAlive(None),
        )?,
        Packet::new(
            PacketType::KeepAlive,
            PacketFlags::NONE,
            TransportSequence::new(2),
            Payload::KeepAlive(Some(VersionData {
                remote_id: RemoteId::new(42),
                service: SERVICE_OPEN_TERMINAL,
                description: vec![0, 0xff, b'R'],
            })),
        )?,
        Packet::new(
            PacketType::Close,
            PacketFlags::NONE,
            TransportSequence::new(3),
            Payload::Close,
        )?,
        Packet::new(
            PacketType::Challenge,
            PacketFlags::NONE,
            TransportSequence::new(4),
            Payload::Challenge(vec![1, 2, 3, 4]),
        )?,
        Packet::new(
            PacketType::Authentication,
            PacketFlags::NONE,
            TransportSequence::new(5),
            Payload::Authentication([0xa5; AUTHENTICATION_LEN]),
        )?,
        Packet::new(
            PacketType::Redirection,
            PacketFlags::NONE,
            TransportSequence::new(6),
            Payload::Redirection(vec![1, 0, 2, 0, 3]),
        )?,
        Packet::new(
            PacketType::Report,
            PacketFlags::NONE,
            TransportSequence::new(7),
            Payload::Report(b"connected".to_vec()),
        )?,
        Packet::new(
            PacketType::BusyNotice,
            PacketFlags::NONE,
            TransportSequence::new(8),
            Payload::Busy(b"busy".to_vec()),
        )?,
    ];

    for packet in cases {
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn open_terminal_control_payloads_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let cases = vec![
        Packet::new(
            PacketType::Subscription,
            PacketFlags::NONE,
            TransportSequence::new(11),
            Payload::Subscription(None),
        )?,
        Packet::new(
            PacketType::Subscription,
            PacketFlags::NONE,
            TransportSequence::new(12),
            Payload::Subscription(Some(Subscription {
                session_type: SessionType::Unknown(UnknownSessionType::new(0xfeed_beef)?),
                target: WireDmrId::new(0x0102_0304),
            })),
        )?,
        Packet::new(
            PacketType::Cancelling,
            PacketFlags::NONE,
            TransportSequence::new(13),
            Payload::Cancelling(None),
        )?,
        Packet::new(
            PacketType::Cancelling,
            PacketFlags::NONE,
            TransportSequence::new(14),
            Payload::Cancelling(Some(Subscription {
                session_type: SessionType::PrivateVoice,
                target: WireDmrId::new(3_102_605),
            })),
        )?,
        Packet::new(
            PacketType::SessionPoll,
            PacketFlags::NONE,
            TransportSequence::new(15),
            Payload::SessionPoll(SessionPoll {
                kind: PollKind::new(1),
                flags: PollFlags::from_bits(2),
                number: WireDmrId::new(3),
                state: PollState::new(4),
            }),
        )?,
    ];

    for packet in cases {
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn modeled_media_payloads_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let link_control = sample_link_control();
    let cases = vec![
        Packet::new(
            PacketType::DmrVoiceHeader,
            PacketFlags::REAL_TIME_1,
            TransportSequence::new(16),
            Payload::DmrVoiceHeader(link_control),
        )?,
        Packet::new(
            PacketType::DmrTerminator,
            PacketFlags::REAL_TIME_1,
            TransportSequence::new(17),
            Payload::DmrTerminator(Some(FullLinkControl {
                flco: ControlOctet::new(3),
                ..link_control
            })),
        )?,
        Packet::new(
            PacketType::DmrTerminator,
            PacketFlags::REAL_TIME_1,
            TransportSequence::new(18),
            Payload::DmrTerminator(None),
        )?,
        Packet::new(
            PacketType::DmrEmbeddedData,
            PacketFlags::REAL_TIME_1,
            TransportSequence::new(19),
            Payload::DmrEmbeddedData(vec![0x5a; 10]),
        )?,
        Packet::new(
            PacketType::SuperHeader,
            PacketFlags::REAL_TIME_1,
            TransportSequence::new(20),
            Payload::SuperHeader(SuperHeader {
                session_type: SessionType::PrivateVoice,
                source_id: WireDmrId::new(123),
                target_id: WireDmrId::new(456),
                source_call: Callsign::from_bytes(*b"SOURCE\0\0\0\0"),
                target_call: Callsign::from_bytes(*b"TARGET\0\0\0\0"),
            }),
        )?,
        Packet::new(
            PacketType::Failure,
            PacketFlags::NONE,
            TransportSequence::new(21),
            Payload::Failure(vec![0xde, 0xad]),
        )?,
    ];

    for packet in cases {
        assert_round_trip(&packet)?;
    }
    for subtype in 0..=6 {
        let packet = Packet::new(
            PacketType::DmrAudio(DmrAudioSubtype::new(subtype)?),
            PacketFlags::from_bits(0x8001),
            TransportSequence::new(u32::from(subtype)),
            Payload::DmrAudio([subtype; DMR_AUDIO_LEN]),
        )?;
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn opaque_packet_classes_and_unknown_types_round_trip() -> Result<(), Box<dyn std::error::Error>> {
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
        PacketType::DmrData(DmrDataSubtype::new(0)?),
        PacketType::DmrData(DmrDataSubtype::new(3)?),
        PacketType::DmrData(DmrDataSubtype::new(15)?),
        PacketType::TerminalIdle,
        PacketType::TerminalAttach,
        PacketType::TerminalDetach,
        PacketType::TerminalWakeup,
        PacketType::MessageText,
        PacketType::MessageStatus,
        PacketType::LocationReport,
        PacketType::LocationRequest,
        PacketType::Unknown(UnknownPacketType::new(0xbeef)?),
    ];

    for packet_type in packet_types {
        let packet = Packet::new(
            packet_type,
            PacketFlags::from_bits(0xf000),
            TransportSequence::new(u32::MAX),
            Payload::Opaque(vec![0, 1, 0xfe, 0xff]),
        )?;
        assert_round_trip(&packet)?;
    }
    Ok(())
}

#[test]
fn unknown_session_values_and_callsign_bytes_are_lossless() -> Result<(), Box<dyn std::error::Error>>
{
    let callsign = Callsign::from_bytes([b'N', b'0', 0xff, b' ', b' ', 0, b'X', b'X', b'X', b'X']);
    assert_eq!(
        callsign.to_bytes(),
        [b'N', b'0', 0xff, b' ', b' ', 0, b'X', b'X', b'X', b'X'],
        "callsign accessor must preserve all ten bytes"
    );
    assert_eq!(
        callsign.to_trimmed_lossy(),
        "N0�",
        "display helper must stop at NUL and trim trailing spaces"
    );

    let packet = Packet::new(
        PacketType::SuperHeader,
        PacketFlags::NONE,
        TransportSequence::new(0),
        Payload::SuperHeader(SuperHeader {
            session_type: SessionType::Unknown(UnknownSessionType::new(0x8765_4321)?),
            source_id: WireDmrId::new(u32::MAX),
            target_id: WireDmrId::new(0x8000_0000),
            source_call: callsign,
            target_call: Callsign::from_bytes([0xff; 10]),
        }),
    )?;
    assert_round_trip(&packet)?;
    Ok(())
}

#[test]
fn independent_wire_domains_preserve_full_width_and_unknown_flags()
-> Result<(), Box<dyn std::error::Error>> {
    for raw in [0, 0x8000_0000, u32::MAX] {
        let poll = SessionPoll {
            kind: PollKind::new(raw),
            flags: PollFlags::from_bits(raw),
            number: WireDmrId::new(raw),
            state: PollState::new(raw),
        };
        let packet = Packet::new(
            PacketType::SessionPoll,
            PacketFlags::NONE,
            TransportSequence::new(raw),
            Payload::SessionPoll(poll),
        )?;
        assert_round_trip(&packet)?;
        assert_eq!(packet.header().sequence.get(), raw);
        assert_eq!(poll.number.get(), raw);
        assert_eq!(u32::from(RemoteId::new(raw)), raw);
        assert_eq!(format!("{:#010x}", poll.number), format!("{raw:#010x}"));
    }
    let mut flags = PollFlags::from_bits(0x8000_0001);
    flags |= PollFlags::from_bits(2);
    flags &= !PollFlags::from_bits(1);
    flags ^= PollFlags::from_bits(4);
    assert_eq!(flags.bits(), 0x8000_0006);
    assert!(flags.contains(PollFlags::from_bits(0x8000_0000)));
    for raw in 0..=u8::MAX {
        let control = ControlOctet::new(raw);
        let options = ServiceOptions::from_bits(raw);
        let mut link = sample_link_control();
        link.flco = control;
        link.feature_id = FeatureId::new(raw);
        link.service_options = options;
        assert_eq!(FullLinkControl::from_bytes(link.to_bytes()), link);
        assert_eq!(control.call_type(), FullLinkControlType::from_flco(raw));
    }
    Ok(())
}

#[test]
fn malformed_envelopes_are_rejected_before_payload_parsing() {
    for length in 0..HEADER_LEN {
        let datagram = vec![0; length];
        assert_eq!(
            Packet::from_bytes(&datagram),
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
        Packet::from_bytes(&invalid_signature),
        Err(CodecError::InvalidSignature),
        "a complete header with the wrong signature must be rejected"
    );

    let truncated = raw_datagram(0x0001, 0, 0, 1, &[]);
    assert_eq!(
        Packet::from_bytes(&truncated),
        Err(CodecError::TruncatedPayload {
            declared: 1,
            actual: 0,
        }),
        "declared bytes missing from the datagram must be rejected"
    );

    let trailing = raw_datagram(0x0001, 0, 0, 0, &[0xff]);
    assert_eq!(
        Packet::from_bytes(&trailing),
        Err(CodecError::TrailingBytes { trailing: 1 }),
        "undeclared trailing bytes must be rejected"
    );

    let oversized = vec![0; MAX_DATAGRAM_LEN + 1];
    assert_eq!(
        Packet::from_bytes(&oversized),
        Err(CodecError::DatagramTooLong {
            actual: MAX_DATAGRAM_LEN + 1,
            maximum: MAX_DATAGRAM_LEN,
        }),
        "datagrams beyond the codec's byte limit must be rejected"
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
        (0x0928, 31),
        (0x0928, 33),
    ];

    for (packet_type, length) in cases {
        let payload = vec![0; length];
        let datagram = exact_datagram(packet_type, &payload);
        assert!(
            matches!(
                Packet::from_bytes(&datagram),
                Err(CodecError::InvalidPayloadLength { .. })
            ),
            "type {packet_type:#06x} must reject malformed payload length {length}"
        );
    }
}

#[test]
fn packet_construction_rejects_mismatched_payloads() {
    assert_eq!(
        Packet::new(
            PacketType::Close,
            PacketFlags::NONE,
            TransportSequence::new(0),
            Payload::Challenge(vec![])
        ),
        Err(CodecError::PayloadTypeMismatch {
            packet_type: PacketType::Close,
        }),
        "constructor must reject a mismatched payload variant"
    );
}

#[test]
fn subtype_construction_rejects_reserved_and_out_of_range_values() {
    const DATA: [u8; 14] = [0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    const AUDIO: [u8; 7] = [0, 1, 2, 3, 4, 5, 6];
    for raw in 0..=u8::MAX {
        assert_eq!(
            DmrDataSubtype::new(raw).is_ok(),
            DATA.contains(&raw),
            "data subtype {raw}"
        );
        assert_eq!(
            DmrAudioSubtype::new(raw).is_ok(),
            AUDIO.contains(&raw),
            "audio subtype {raw}"
        );
        assert_eq!(
            UnknownFullLinkControlType::new(raw).is_ok(),
            (1..=63).contains(&raw) && raw != 3,
            "unknown opcode {raw}",
        );
    }
}

#[test]
fn unknown_codes_cannot_alias_named_interpretations() -> Result<(), Box<dyn std::error::Error>> {
    for raw in [
        0x0000, 0x0911, 0x0912, 0x091f, 0x0920, 0x0926, 0x0927, 0x0a21,
    ] {
        assert_eq!(
            UnknownPacketType::new(raw),
            Err(CodecError::KnownPacketType { raw })
        );
    }
    for raw in [5, 7] {
        assert_eq!(
            UnknownSessionType::new(raw),
            Err(CodecError::KnownSessionType { raw })
        );
    }
    let unknown = UnknownPacketType::new(0xbeef)?;
    assert_eq!(u16::from(unknown), 0xbeef);
    for raw in 0..=u16::MAX {
        assert_eq!(
            PacketType::from_raw(raw).as_raw(),
            raw,
            "packet code {raw:#06x}"
        );
    }
    Ok(())
}

#[test]
fn full_link_control_rejects_ids_larger_than_24_bits() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(DmrId::new(0)?.get(), 0);
    assert_eq!(DmrId::new(0x00ff_ffff)?, DmrId::MAX);
    for value in [0x0100_0000, u32::MAX] {
        assert_eq!(
            DmrId::new(value),
            Err(CodecError::DmrIdOutOfRange { value }),
            "identifier {value} must fit three wire bytes",
        );
    }
    Ok(())
}

#[test]
fn dmr_identifiers_support_wire_inspection_formats() -> Result<(), Box<dyn std::error::Error>> {
    let id = DmrId::new(0x12_34_ab)?;
    assert_eq!(format!("{id}"), "1193131");
    assert_eq!(format!("{id:#08x}"), "0x1234ab");
    assert_eq!(format!("{id:06X}"), "1234AB");
    assert_eq!(format!("{id:o}"), "4432253");
    assert_eq!(format!("{id:024b}"), "000100100011010010101011");
    Ok(())
}

#[test]
fn codec_enforces_its_datagram_size_limit() -> Result<(), Box<dyn std::error::Error>> {
    let maximum = Packet::new(
        PacketType::Unknown(UnknownPacketType::new(0xbeef)?),
        PacketFlags::NONE,
        TransportSequence::new(0),
        Payload::Opaque(vec![0x5a; MAX_PAYLOAD_LEN]),
    )?;
    let encoded = maximum.to_bytes();
    assert_eq!(
        encoded.len(),
        MAX_DATAGRAM_LEN,
        "the largest accepted payload must fill the configured datagram limit"
    );
    assert_eq!(
        Packet::from_bytes(&encoded)?,
        maximum,
        "the largest accepted datagram must round-trip"
    );

    assert_eq!(
        Packet::new(
            PacketType::Unknown(UnknownPacketType::new(0xbeef)?),
            PacketFlags::NONE,
            TransportSequence::new(0),
            Payload::Opaque(vec![0; MAX_PAYLOAD_LEN + 1])
        ),
        Err(CodecError::PayloadTooLong {
            actual: MAX_PAYLOAD_LEN + 1,
            maximum: MAX_PAYLOAD_LEN,
        }),
        "payload one byte over the codec limit must be rejected"
    );
    Ok(())
}

#[test]
fn version_description_limit_includes_its_fixed_prefix() -> Result<(), Box<dyn std::error::Error>> {
    let description = vec![0x5a; MAX_PAYLOAD_LEN - 5];
    let allocation = description.as_ptr();
    let packet = Packet::new(
        PacketType::KeepAlive,
        PacketFlags::NONE,
        TransportSequence::new(17),
        Payload::KeepAlive(Some(VersionData {
            remote_id: RemoteId::new(u32::MAX),
            service: SERVICE_OPEN_TERMINAL,
            description,
        })),
    )?;
    let mut header_copy = packet.header();
    header_copy.payload_len = 0;
    assert_eq!(usize::from(packet.header().payload_len), MAX_PAYLOAD_LEN);
    assert_ne!(
        header_copy,
        packet.header(),
        "envelope copies must be independent"
    );
    let encoded = packet.to_bytes();
    assert_eq!(encoded.len(), MAX_DATAGRAM_LEN);
    assert_eq!(Packet::from_bytes(&encoded)?, packet);
    let Payload::KeepAlive(Some(version)) = packet.into_payload() else {
        return Err("expected version data".into());
    };
    assert_eq!(
        version.description.as_ptr(),
        allocation,
        "construction and consumption must transfer storage"
    );
    assert_eq!(
        version.remote_id.get(),
        u32::MAX,
        "version identifiers retain all wire bits"
    );
    assert_eq!(
        Packet::new(
            PacketType::KeepAlive,
            PacketFlags::NONE,
            TransportSequence::new(18),
            Payload::KeepAlive(Some(VersionData {
                remote_id: RemoteId::new(0),
                service: SERVICE_OPEN_TERMINAL,
                description: vec![0; MAX_PAYLOAD_LEN - 4],
            })),
        ),
        Err(CodecError::PayloadTooLong {
            actual: MAX_PAYLOAD_LEN + 1,
            maximum: MAX_PAYLOAD_LEN,
        }),
        "the five-byte version prefix counts toward the payload limit",
    );
    Ok(())
}

#[test]
fn embedded_data_preserves_variable_bodies_with_packet_bounds()
-> Result<(), Box<dyn std::error::Error>> {
    for length in [0, 1, 9, 10, 11, MAX_PAYLOAD_LEN] {
        let bytes = vec![0xa5; length];
        let datagram = exact_datagram(0x0927, &bytes);
        let packet = Packet::from_bytes(&datagram)?;
        assert_eq!(
            packet.payload(),
            &Payload::DmrEmbeddedData(bytes),
            "embedded body length {length}"
        );
        assert_eq!(
            packet.to_bytes(),
            datagram,
            "embedded bytes must be lossless at length {length}"
        );
    }
    assert_eq!(
        Packet::new(
            PacketType::DmrEmbeddedData,
            PacketFlags::NONE,
            TransportSequence::new(0),
            Payload::DmrEmbeddedData(vec![0; MAX_PAYLOAD_LEN + 1])
        ),
        Err(CodecError::PayloadTooLong {
            actual: MAX_PAYLOAD_LEN + 1,
            maximum: MAX_PAYLOAD_LEN
        }),
        "embedded bodies retain the outer packet bound",
    );
    Ok(())
}

#[test]
fn arbitrary_short_inputs_return_errors_without_panicking() {
    for length in 0..=256 {
        let input = vec![0xa5; length];
        assert!(
            Packet::from_bytes(&input).is_err(),
            "non-REWIND input of length {length} must return an error"
        );
    }
}
