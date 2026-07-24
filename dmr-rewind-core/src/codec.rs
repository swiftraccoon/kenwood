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
    let sequence = read_u32_le(datagram, 12)?;
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

pub(super) fn encode(packet: &Packet) -> Result<Vec<u8>, CodecError> {
    validate_packet_type(packet.header.packet_type)?;
    let payload = encode_payload(packet.header.packet_type, &packet.payload)?;
    let payload_len = payload.len();

    if payload_len > MAX_PAYLOAD_LEN {
        return Err(CodecError::PayloadTooLong {
            actual: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }

    let declared = usize::from(packet.header.payload_len);
    if declared != payload_len {
        return Err(CodecError::HeaderLengthMismatch {
            declared,
            actual: payload_len,
        });
    }

    let mut datagram = Vec::with_capacity(HEADER_LEN + payload_len);
    datagram.extend_from_slice(&SIGNATURE);
    datagram.extend_from_slice(&packet.header.packet_type.as_raw().to_le_bytes());
    datagram.extend_from_slice(&packet.header.flags.bits().to_le_bytes());
    datagram.extend_from_slice(&packet.header.sequence.to_le_bytes());
    datagram.extend_from_slice(&packet.header.payload_len.to_le_bytes());
    datagram.extend_from_slice(&payload);
    Ok(datagram)
}

pub(super) fn validate_and_measure(
    packet_type: PacketType,
    payload: &Payload,
) -> Result<usize, CodecError> {
    validate_packet_type(packet_type)?;
    let encoded = encode_payload(packet_type, payload)?;
    let payload_len = encoded.len();
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
        PacketType::Subscription => decode_subscription(packet_type, bytes, false),
        PacketType::Cancelling => decode_subscription(packet_type, bytes, true),
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
        PacketType::DmrEmbeddedData => Ok(Payload::DmrEmbeddedData(read_exact_payload(
            packet_type,
            bytes,
            "10",
        )?)),
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
        remote_id,
        service,
        description,
    })))
}

fn decode_subscription(
    packet_type: PacketType,
    bytes: &[u8],
    cancelling: bool,
) -> Result<Payload, CodecError> {
    let value = match bytes.len() {
        0 => None,
        8 => Some(Subscription {
            session_type: SessionType::from_raw(read_u32_le(bytes, 0)?),
            target: read_u32_le(bytes, 4)?,
        }),
        actual => {
            return Err(CodecError::InvalidPayloadLength {
                packet_type,
                expected: "0 or 8",
                actual,
            });
        }
    };

    if cancelling {
        Ok(Payload::Cancelling(value))
    } else {
        Ok(Payload::Subscription(value))
    }
}

fn decode_session_poll(bytes: &[u8]) -> Result<Payload, CodecError> {
    require_len(PacketType::SessionPoll, bytes, 16, "16")?;
    Ok(Payload::SessionPoll(SessionPoll {
        kind: read_u32_le(bytes, 0)?,
        flags: read_u32_le(bytes, 4)?,
        number: read_u32_le(bytes, 8)?,
        state: read_u32_le(bytes, 12)?,
    }))
}

fn decode_super_header(bytes: &[u8]) -> Result<Payload, CodecError> {
    require_len(PacketType::SuperHeader, bytes, SUPER_HEADER_LEN, "32")?;
    Ok(Payload::SuperHeader(SuperHeader {
        session_type: SessionType::from_raw(read_u32_le(bytes, 0)?),
        source_id: read_u32_le(bytes, 4)?,
        target_id: read_u32_le(bytes, 8)?,
        source_call: Callsign::from_raw(read_array(bytes, 12)?),
        target_call: Callsign::from_raw(read_array(bytes, 22)?),
    }))
}

fn encode_payload(packet_type: PacketType, payload: &Payload) -> Result<Vec<u8>, CodecError> {
    let encoded = match (packet_type, payload) {
        (PacketType::KeepAlive, Payload::KeepAlive(value)) => encode_keep_alive(value.as_ref()),
        (PacketType::Close, Payload::Close) => Vec::new(),
        (PacketType::Challenge, Payload::Challenge(value))
        | (PacketType::Redirection, Payload::Redirection(value))
        | (PacketType::Report, Payload::Report(value))
        | (PacketType::BusyNotice, Payload::Busy(value))
        | (PacketType::Failure, Payload::Failure(value)) => value.clone(),
        (PacketType::Authentication, Payload::Authentication(value)) => value.to_vec(),
        (PacketType::Subscription, Payload::Subscription(value))
        | (PacketType::Cancelling, Payload::Cancelling(value)) => encode_subscription(*value),
        (PacketType::SessionPoll, Payload::SessionPoll(value)) => encode_session_poll(*value),
        (PacketType::DmrVoiceHeader, Payload::DmrVoiceHeader(value)) => value.to_bytes()?.to_vec(),
        (PacketType::DmrTerminator, Payload::DmrTerminator(value)) => match value {
            Some(link_control) => link_control.to_bytes()?.to_vec(),
            None => Vec::new(),
        },
        (PacketType::DmrAudio(_), Payload::DmrAudio(value)) => value.to_vec(),
        (PacketType::DmrEmbeddedData, Payload::DmrEmbeddedData(value)) => value.to_vec(),
        (PacketType::SuperHeader, Payload::SuperHeader(value)) => encode_super_header(*value),
        (opaque_type, Payload::Opaque(value)) if accepts_opaque(opaque_type) => value.clone(),
        _ => return Err(CodecError::PayloadTypeMismatch { packet_type }),
    };
    Ok(encoded)
}

fn encode_keep_alive(value: Option<&VersionData>) -> Vec<u8> {
    let Some(version) = value else {
        return Vec::new();
    };

    let mut encoded = Vec::with_capacity(5 + version.description.len());
    encoded.extend_from_slice(&version.remote_id.to_le_bytes());
    encoded.push(version.service);
    encoded.extend_from_slice(&version.description);
    encoded
}

fn encode_subscription(value: Option<Subscription>) -> Vec<u8> {
    let Some(subscription) = value else {
        return Vec::new();
    };

    let mut encoded = Vec::with_capacity(8);
    encoded.extend_from_slice(&subscription.session_type.as_raw().to_le_bytes());
    encoded.extend_from_slice(&subscription.target.to_le_bytes());
    encoded
}

fn encode_session_poll(value: SessionPoll) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(16);
    encoded.extend_from_slice(&value.kind.to_le_bytes());
    encoded.extend_from_slice(&value.flags.to_le_bytes());
    encoded.extend_from_slice(&value.number.to_le_bytes());
    encoded.extend_from_slice(&value.state.to_le_bytes());
    encoded
}

fn encode_super_header(value: SuperHeader) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(SUPER_HEADER_LEN);
    encoded.extend_from_slice(&value.session_type.as_raw().to_le_bytes());
    encoded.extend_from_slice(&value.source_id.to_le_bytes());
    encoded.extend_from_slice(&value.target_id.to_le_bytes());
    encoded.extend_from_slice(&value.source_call.into_raw());
    encoded.extend_from_slice(&value.target_call.into_raw());
    encoded
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

fn validate_packet_type(packet_type: PacketType) -> Result<(), CodecError> {
    match packet_type {
        PacketType::DmrData(subtype) if subtype > 0x0f => Err(CodecError::InvalidSubtype {
            packet_type,
            subtype,
        }),
        PacketType::DmrData(1 | 2) => Err(CodecError::NonCanonicalPacketType { packet_type }),
        PacketType::DmrAudio(subtype) if subtype > 6 => Err(CodecError::InvalidSubtype {
            packet_type,
            subtype,
        }),
        PacketType::Unknown(raw) if PacketType::from_raw(raw) != packet_type => {
            Err(CodecError::NonCanonicalPacketType { packet_type })
        }
        _ => Ok(()),
    }
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
