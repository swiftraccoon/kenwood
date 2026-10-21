// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

use super::{
    Callsign, CodecError, FULL_LINK_CONTROL_LEN, FullLinkControl, HEADER_LEN, Header,
    MAX_DATAGRAM_LEN, MAX_PAYLOAD_LEN, Packet, PacketFlags, PacketType, Payload, SIGNATURE,
    SUPER_HEADER_LEN, SessionPoll, SessionType, Subscription, SuperHeader, VersionData,
};

pub(super) fn decode(datagram: &[u8]) -> Result<Packet, CodecError> {
    let datagram_len = datagram.len();
    if datagram_len < HEADER_LEN {
        return Err(CodecError::DatagramTooShort {
            actual: datagram_len,
            minimum: HEADER_LEN,
        });
    }
    if datagram_len > MAX_DATAGRAM_LEN {
        return Err(CodecError::DatagramTooLong {
            actual: datagram_len,
            maximum: MAX_DATAGRAM_LEN,
        });
    }

    let signature = datagram
        .get(..SIGNATURE.len())
        .ok_or(CodecError::DatagramTooShort {
            actual: datagram_len,
            minimum: HEADER_LEN,
        })?;
    if signature != SIGNATURE {
        return Err(CodecError::InvalidSignature);
    }

    let packet_type = PacketType::from_raw(read_u16_le(datagram, 8)?);
    let flags = PacketFlags::from_bits(read_u16_le(datagram, 10)?);
    let sequence = read_u32_le(datagram, 12)?.into();
    let declared = usize::from(read_u16_le(datagram, 16)?);
    let actual = datagram_len.saturating_sub(HEADER_LEN);

    if actual < declared {
        return Err(CodecError::TruncatedPayload { declared, actual });
    }
    if actual > declared {
        return Err(CodecError::TrailingBytes {
            trailing: actual - declared,
        });
    }

    let payload_bytes = datagram
        .get(HEADER_LEN..)
        .ok_or(CodecError::DatagramTooShort {
            actual: datagram_len,
            minimum: HEADER_LEN,
        })?;
    let payload = decode_payload(packet_type, payload_bytes)?;

    Ok(Packet {
        header: Header {
            packet_type,
            flags,
            sequence,
            payload_len: u16::try_from(declared).map_err(|_| CodecError::PayloadTooLong {
                actual: declared,
                maximum: MAX_PAYLOAD_LEN,
            })?,
        },
        payload,
    })
}

pub(super) fn encode(packet: &Packet) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(HEADER_LEN + usize::from(packet.header.payload_len));
    datagram.extend_from_slice(&SIGNATURE);
    datagram.extend_from_slice(&packet.header.packet_type.as_raw().to_le_bytes());
    datagram.extend_from_slice(&packet.header.flags.bits().to_le_bytes());
    datagram.extend_from_slice(&packet.header.sequence.get().to_le_bytes());
    datagram.extend_from_slice(&packet.header.payload_len.to_le_bytes());
    encode_payload(&packet.payload, &mut datagram);
    datagram
}

pub(super) fn validate_and_measure(
    packet_type: PacketType,
    payload: &Payload,
) -> Result<usize, CodecError> {
    let payload_len = match (packet_type, payload) {
        (PacketType::KeepAlive, Payload::KeepAlive(value)) => value
            .as_ref()
            .map_or(0, |version| version.description.len().saturating_add(5)),
        (PacketType::Close, Payload::Close) => 0,
        (PacketType::Challenge, Payload::Challenge(value))
        | (PacketType::Redirection, Payload::Redirection(value))
        | (PacketType::Report, Payload::Report(value))
        | (PacketType::BusyNotice, Payload::Busy(value))
        | (PacketType::Failure, Payload::Failure(value))
        | (PacketType::DmrEmbeddedData, Payload::DmrEmbeddedData(value)) => value.len(),
        (PacketType::Authentication, Payload::Authentication(value)) => value.len(),
        (PacketType::Subscription, Payload::Subscription(value))
        | (PacketType::Cancelling, Payload::Cancelling(value)) => {
            if value.is_some() {
                8
            } else {
                0
            }
        }
        (PacketType::SessionPoll, Payload::SessionPoll(_)) => 16,
        (PacketType::DmrVoiceHeader, Payload::DmrVoiceHeader(_)) => FULL_LINK_CONTROL_LEN,
        (PacketType::DmrTerminator, Payload::DmrTerminator(value)) => {
            if value.is_some() {
                FULL_LINK_CONTROL_LEN
            } else {
                0
            }
        }
        (PacketType::DmrAudio(_), Payload::DmrAudio(value)) => value.len(),
        (PacketType::SuperHeader, Payload::SuperHeader(_)) => SUPER_HEADER_LEN,
        (opaque_type, Payload::Opaque(value)) if accepts_opaque(opaque_type) => value.len(),
        _ => return Err(CodecError::PayloadTypeMismatch { packet_type }),
    };
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(CodecError::PayloadTooLong {
            actual: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    Ok(payload_len)
}

fn decode_payload(packet_type: PacketType, bytes: &[u8]) -> Result<Payload, CodecError> {
    match packet_type {
        PacketType::KeepAlive => decode_keep_alive(bytes),
        PacketType::Close => {
            require_len(packet_type, bytes, 0, "0")?;
            Ok(Payload::Close)
        }
        PacketType::Challenge => Ok(Payload::Challenge(bytes.to_vec())),
        PacketType::Authentication => Ok(Payload::Authentication(read_exact_payload(
            packet_type,
            bytes,
            "32",
        )?)),
        PacketType::Redirection => Ok(Payload::Redirection(bytes.to_vec())),
        PacketType::Report => Ok(Payload::Report(bytes.to_vec())),
        PacketType::BusyNotice => Ok(Payload::Busy(bytes.to_vec())),
        PacketType::Subscription | PacketType::Cancelling => {
            decode_subscription(packet_type, bytes)
        }
        PacketType::SessionPoll => decode_session_poll(bytes),
        PacketType::DmrVoiceHeader => {
            let raw: [u8; FULL_LINK_CONTROL_LEN] = read_exact_payload(packet_type, bytes, "12")?;
            Ok(Payload::DmrVoiceHeader(FullLinkControl::from_bytes(raw)))
        }
        PacketType::DmrTerminator => {
            let link_control = match bytes.len() {
                0 => None,
                FULL_LINK_CONTROL_LEN => {
                    let raw: [u8; FULL_LINK_CONTROL_LEN] = read_array(bytes, 0)?;
                    Some(FullLinkControl::from_bytes(raw))
                }
                actual => {
                    return Err(CodecError::InvalidPayloadLength {
                        packet_type,
                        expected: "0 or 12",
                        actual,
                    });
                }
            };
            Ok(Payload::DmrTerminator(link_control))
        }
        PacketType::DmrAudio(_) => Ok(Payload::DmrAudio(read_exact_payload(
            packet_type,
            bytes,
            "27",
        )?)),
        PacketType::DmrEmbeddedData => Ok(Payload::DmrEmbeddedData(bytes.to_vec())),
        PacketType::SuperHeader => decode_super_header(bytes),
        PacketType::Failure => Ok(Payload::Failure(bytes.to_vec())),
        _ => Ok(Payload::Opaque(bytes.to_vec())),
    }
}

fn decode_keep_alive(bytes: &[u8]) -> Result<Payload, CodecError> {
    if bytes.is_empty() {
        return Ok(Payload::KeepAlive(None));
    }
    if bytes.len() < 5 {
        return Err(CodecError::InvalidPayloadLength {
            packet_type: PacketType::KeepAlive,
            expected: "0 or at least 5",
            actual: bytes.len(),
        });
    }

    let remote_id = read_u32_le(bytes, 0)?;
    let service = bytes
        .get(4)
        .copied()
        .ok_or(CodecError::InvalidPayloadLength {
            packet_type: PacketType::KeepAlive,
            expected: "0 or at least 5",
            actual: bytes.len(),
        })?;
    let description = bytes
        .get(5..)
        .ok_or(CodecError::InvalidPayloadLength {
            packet_type: PacketType::KeepAlive,
            expected: "0 or at least 5",
            actual: bytes.len(),
        })?
        .to_vec();

    Ok(Payload::KeepAlive(Some(VersionData {
        remote_id: remote_id.into(),
        service: service.into(),
        description,
    })))
}

fn decode_subscription(packet_type: PacketType, bytes: &[u8]) -> Result<Payload, CodecError> {
    let value = match bytes.len() {
        0 => None,
        8 => Some(Subscription {
            session_type: SessionType::from_raw(read_u32_le(bytes, 0)?),
            target: read_u32_le(bytes, 4)?.into(),
        }),
        actual => {
            return Err(CodecError::InvalidPayloadLength {
                packet_type,
                expected: "0 or 8",
                actual,
            });
        }
    };

    if packet_type == PacketType::Cancelling {
        Ok(Payload::Cancelling(value))
    } else {
        Ok(Payload::Subscription(value))
    }
}

fn decode_session_poll(bytes: &[u8]) -> Result<Payload, CodecError> {
    require_len(PacketType::SessionPoll, bytes, 16, "16")?;
    Ok(Payload::SessionPoll(SessionPoll {
        kind: read_u32_le(bytes, 0)?.into(),
        flags: read_u32_le(bytes, 4)?.into(),
        number: read_u32_le(bytes, 8)?.into(),
        state: read_u32_le(bytes, 12)?.into(),
    }))
}

fn decode_super_header(bytes: &[u8]) -> Result<Payload, CodecError> {
    require_len(PacketType::SuperHeader, bytes, SUPER_HEADER_LEN, "32")?;
    Ok(Payload::SuperHeader(SuperHeader {
        session_type: SessionType::from_raw(read_u32_le(bytes, 0)?),
        source_id: read_u32_le(bytes, 4)?.into(),
        target_id: read_u32_le(bytes, 8)?.into(),
        source_call: Callsign::from_bytes(read_array(bytes, 12)?),
        target_call: Callsign::from_bytes(read_array(bytes, 22)?),
    }))
}

fn encode_payload(payload: &Payload, encoded: &mut Vec<u8>) {
    match payload {
        Payload::KeepAlive(Some(version)) => {
            encoded.extend_from_slice(&version.remote_id.get().to_le_bytes());
            encoded.push(version.service.get());
            encoded.extend_from_slice(&version.description);
        }
        Payload::KeepAlive(None)
        | Payload::Close
        | Payload::Subscription(None)
        | Payload::Cancelling(None)
        | Payload::DmrTerminator(None) => {}
        Payload::Challenge(value)
        | Payload::Redirection(value)
        | Payload::Report(value)
        | Payload::Busy(value)
        | Payload::Failure(value)
        | Payload::Opaque(value) => {
            encoded.extend_from_slice(value);
        }
        Payload::Authentication(value) => encoded.extend_from_slice(value),
        Payload::Subscription(Some(value)) | Payload::Cancelling(Some(value)) => {
            encoded.extend_from_slice(&value.session_type.as_raw().to_le_bytes());
            encoded.extend_from_slice(&value.target.get().to_le_bytes());
        }
        Payload::SessionPoll(value) => {
            encoded.extend_from_slice(&value.kind.get().to_le_bytes());
            encoded.extend_from_slice(&value.flags.bits().to_le_bytes());
            encoded.extend_from_slice(&value.number.get().to_le_bytes());
            encoded.extend_from_slice(&value.state.get().to_le_bytes());
        }
        Payload::DmrVoiceHeader(value) | Payload::DmrTerminator(Some(value)) => {
            encoded.extend_from_slice(&value.to_bytes());
        }
        Payload::DmrAudio(value) => encoded.extend_from_slice(value),
        Payload::DmrEmbeddedData(value) => encoded.extend_from_slice(value),
        Payload::SuperHeader(value) => {
            encoded.extend_from_slice(&value.session_type.as_raw().to_le_bytes());
            encoded.extend_from_slice(&value.source_id.get().to_le_bytes());
            encoded.extend_from_slice(&value.target_id.get().to_le_bytes());
            encoded.extend_from_slice(&value.source_call.to_bytes());
            encoded.extend_from_slice(&value.target_call.to_bytes());
        }
    }
}

const fn accepts_opaque(packet_type: PacketType) -> bool {
    matches!(
        packet_type,
        PacketType::Configuration
            | PacketType::AddressNotice
            | PacketType::BindingNotice
            | PacketType::ExternalServer
            | PacketType::RemoteControl
            | PacketType::SnmpTrap
            | PacketType::PeerData
            | PacketType::RdacData
            | PacketType::MediaData
            | PacketType::DmrData(_)
            | PacketType::TerminalIdle
            | PacketType::TerminalAttach
            | PacketType::TerminalDetach
            | PacketType::TerminalWakeup
            | PacketType::MessageText
            | PacketType::MessageStatus
            | PacketType::LocationReport
            | PacketType::LocationRequest
            | PacketType::Unknown(_)
    )
}

const fn require_len(
    packet_type: PacketType,
    bytes: &[u8],
    expected_len: usize,
    expected: &'static str,
) -> Result<(), CodecError> {
    if bytes.len() == expected_len {
        Ok(())
    } else {
        Err(CodecError::InvalidPayloadLength {
            packet_type,
            expected,
            actual: bytes.len(),
        })
    }
}

fn read_exact_payload<const N: usize>(
    packet_type: PacketType,
    bytes: &[u8],
    expected: &'static str,
) -> Result<[u8; N], CodecError> {
    require_len(packet_type, bytes, N, expected)?;
    read_array(bytes, 0)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, CodecError> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, CodecError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], CodecError> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| CodecError::DatagramTooShort {
            actual: bytes.len(),
            minimum: offset.saturating_add(N),
        })?;
    let value = bytes.get(offset..end).ok_or(CodecError::DatagramTooShort {
        actual: bytes.len(),
        minimum: end,
    })?;
    value.try_into().map_err(|_| CodecError::DatagramTooShort {
        actual: bytes.len(),
        minimum: end,
    })
}
