//! `DExtra` packet decoders.
//!
//! Both directions — `decode_client_to_server` parses packets a
//! client would send, `decode_server_to_client` parses packets a
//! reflector would send.
//!
//! Lenient: recoverable malformations push `Diagnostic`s to the
//! supplied sink but still return a parsed packet. Only fatal errors
//! (wrong length, missing magic, zero stream id, invalid module byte)
//! return `Err`.

use crate::header::{DStarHeader, ENCODED_LEN};
use crate::types::{Callsign, Module, ProtocolKind, StreamId};
use crate::validator::{Diagnostic, DiagnosticSink};
use crate::voice::VoiceFrame;

use super::consts::{
    CONNECT_ACK_TAG, CONNECT_LEN, CONNECT_NAK_TAG, CONNECT_REPLY_LEN, DSVT_MAGIC, POLL_LEN,
    VOICE_DATA_LEN, VOICE_HEADER_LEN,
};
use super::error::DExtraError;
use super::packet::{ClientPacket, ServerPacket};

/// Decode a UDP datagram sent from a `DExtra` client (client → server).
///
/// # Errors
///
/// - [`DExtraError::UnknownPacketLength`] for unrecognized lengths
/// - [`DExtraError::DsvtMagicMissing`] for voice-length packets without DSVT magic
/// - [`DExtraError::StreamIdZero`] for voice packets with stream id 0
/// - [`DExtraError::InvalidModuleByte`] for LINK/UNLINK with a non-A-Z
///   module byte at `[8]` or `[9]` (UNLINK still requires byte `[9]` =
///   `b' '` which is handled separately).
///
/// # See also
///
/// `ircDDBGateway/Common/DExtraProtocolHandler.cpp` — the reference
/// parser this decoder mirrors. `xlxd/src/cdextraprotocol.cpp` is
/// a mirror reference.
pub fn decode_client_to_server(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DExtraError> {
    let len = bytes.len();
    match len {
        POLL_LEN => {
            let cs = extract_callsign(bytes);
            Ok(ClientPacket::Poll { callsign: cs })
        }
        CONNECT_LEN => decode_client_connect(bytes),
        VOICE_DATA_LEN => decode_dsvt_client_voice(bytes, sink),
        VOICE_HEADER_LEN => decode_dsvt_client_header(bytes, sink),
        _ => Err(DExtraError::UnknownPacketLength { got: len }),
    }
}

/// Decode a UDP datagram sent from a `DExtra` reflector (server → client).
///
/// # Errors
///
/// Same as [`decode_client_to_server`], but produces [`ServerPacket`]
/// variants. The 14-byte ACK/NAK reply length is only accepted here.
///
/// # See also
///
/// `ircDDBGateway/Common/DExtraProtocolHandler.cpp` — the reference
/// parser for the server-side `DExtra` wire format.
pub fn decode_server_to_client(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DExtraError> {
    let len = bytes.len();
    match len {
        POLL_LEN => {
            let cs = extract_callsign(bytes);
            Ok(ServerPacket::PollEcho { callsign: cs })
        }
        CONNECT_REPLY_LEN => decode_server_connect_reply(bytes),
        VOICE_DATA_LEN => decode_dsvt_server_voice(bytes, sink),
        VOICE_HEADER_LEN => decode_dsvt_server_header(bytes, sink),
        _ => Err(DExtraError::UnknownPacketLength { got: len }),
    }
}

/// Extract an 8-byte callsign from a packet prefix (positions `[0..8]`).
///
/// Callers must have already verified that the slice is at least 8
/// bytes long. Any zeros in the callsign field are normalized to
/// spaces so that `Callsign::from_wire_bytes` records a clean wire
/// representation.
///
/// On the connect-packet wire format, byte `[7]` is the pad slot
/// (space from `memset`) and byte `[8]` — outside the 8-byte
/// window — holds the module letter per
/// `ircDDBGateway/Common/ConnectData.cpp:278-300` (`getDExtraData`).
/// Our API exposes the module as a separate `Module` field on
/// [`ClientPacket::Link`]/[`ServerPacket::ConnectAck`], so this
/// reader deliberately does NOT splice byte `[8]` into the
/// callsign. Keeping byte `[7]` as the plain space from the wire
/// matches what the 9-byte poll packet decoder sees and lets the
/// server session compare stored callsigns against incoming polls
/// byte-for-byte.
fn extract_callsign(bytes: &[u8]) -> Callsign {
    let mut cs = [b' '; 8];
    if let Some(src) = bytes.get(..8) {
        cs.copy_from_slice(src);
    }
    for b in &mut cs {
        if *b == 0 {
            *b = b' ';
        }
    }
    Callsign::from_wire_bytes(cs)
}

/// Decode an 11-byte LINK or UNLINK client packet.
fn decode_client_connect(bytes: &[u8]) -> Result<ClientPacket, DExtraError> {
    let cs = extract_callsign(bytes);
    let client_byte = bytes.get(8).copied().unwrap_or(0);
    let client_module =
        Module::try_from_byte(client_byte).map_err(|_| DExtraError::InvalidModuleByte {
            offset: 8,
            byte: client_byte,
        })?;
    let reflector_byte = bytes.get(9).copied().unwrap_or(0);
    if reflector_byte == b' ' {
        Ok(ClientPacket::Unlink {
            callsign: cs,
            client_module,
        })
    } else {
        let reflector_module =
            Module::try_from_byte(reflector_byte).map_err(|_| DExtraError::InvalidModuleByte {
                offset: 9,
                byte: reflector_byte,
            })?;
        Ok(ClientPacket::Link {
            callsign: cs,
            reflector_module,
            client_module,
        })
    }
}

/// Decode a 14-byte ACK or NAK reply from the server.
fn decode_server_connect_reply(bytes: &[u8]) -> Result<ServerPacket, DExtraError> {
    let cs = extract_callsign(bytes);
    // Position [9] carries the reflector module letter.
    let module_byte = bytes.get(9).copied().unwrap_or(0);
    let reflector_module =
        Module::try_from_byte(module_byte).map_err(|_| DExtraError::InvalidModuleByte {
            offset: 9,
            byte: module_byte,
        })?;
    // Tag is at [10..13], NUL at [13] — per
    // `ircDDBGateway/Common/ConnectData.cpp:302-316`.
    let mut tag = [0u8; 3];
    if let Some(src) = bytes.get(10..13) {
        tag.copy_from_slice(src);
    }
    if tag == CONNECT_ACK_TAG {
        Ok(ServerPacket::ConnectAck {
            callsign: cs,
            reflector_module,
        })
    } else if tag == CONNECT_NAK_TAG {
        Ok(ServerPacket::ConnectNak {
            callsign: cs,
            reflector_module,
        })
    } else {
        Err(DExtraError::UnknownConnectTag { tag })
    }
}

/// Decode a 27-byte voice data/EOT packet from the client.
fn decode_dsvt_client_voice(
    bytes: &[u8],
    _sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DExtraError> {
    let (stream_id, seq, frame) = parse_dsvt_voice(bytes)?;
    if seq & 0x40 != 0 {
        Ok(ClientPacket::VoiceEot { stream_id, seq })
    } else {
        Ok(ClientPacket::VoiceData {
            stream_id,
            seq,
            frame,
        })
    }
}

/// Decode a 27-byte voice data/EOT packet from the server.
fn decode_dsvt_server_voice(
    bytes: &[u8],
    _sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DExtraError> {
    let (stream_id, seq, frame) = parse_dsvt_voice(bytes)?;
    if seq & 0x40 != 0 {
        Ok(ServerPacket::VoiceEot { stream_id, seq })
    } else {
        Ok(ServerPacket::VoiceData {
            stream_id,
            seq,
            frame,
        })
    }
}

/// Decode a 56-byte voice header packet from the client.
fn decode_dsvt_client_header(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DExtraError> {
    let (stream_id, header) = parse_dsvt_header(bytes, sink)?;
    Ok(ClientPacket::VoiceHeader { stream_id, header })
}

/// Decode a 56-byte voice header packet from the server.
fn decode_dsvt_server_header(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DExtraError> {
    let (stream_id, header) = parse_dsvt_header(bytes, sink)?;
    Ok(ServerPacket::VoiceHeader { stream_id, header })
}

/// Shared parser for the 27-byte voice data / EOT packet shape.
fn parse_dsvt_voice(bytes: &[u8]) -> Result<(StreamId, u8, VoiceFrame), DExtraError> {
    check_dsvt_magic(bytes)?;
    // Byte [4] must be 0x20 (voice type) for data/EOT.
    if bytes.get(4).copied() != Some(0x20) {
        return Err(DExtraError::UnknownPacketLength { got: bytes.len() });
    }
    let stream_id = extract_stream_id(bytes)?;
    let seq = bytes.get(14).copied().unwrap_or(0);
    let mut ambe = [0u8; 9];
    let mut slow = [0u8; 3];
    if let Some(src) = bytes.get(15..24) {
        ambe.copy_from_slice(src);
    }
    if let Some(src) = bytes.get(24..27) {
        slow.copy_from_slice(src);
    }
    Ok((
        stream_id,
        seq,
        VoiceFrame {
            ambe,
            slow_data: slow,
        },
    ))
}

/// Shared parser for the 56-byte voice header packet shape.
fn parse_dsvt_header(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<(StreamId, DStarHeader), DExtraError> {
    check_dsvt_magic(bytes)?;
    // Byte [4] must be 0x10 (header type) for a voice header.
    if bytes.get(4).copied() != Some(0x10) {
        return Err(DExtraError::UnknownPacketLength { got: bytes.len() });
    }
    let stream_id = extract_stream_id(bytes)?;
    let hdr_slice = bytes
        .get(15..56)
        .ok_or(DExtraError::UnknownPacketLength { got: bytes.len() })?;
    let mut arr = [0u8; ENCODED_LEN];
    arr.copy_from_slice(hdr_slice);
    let decoded = DStarHeader::decode(&arr);
    if decoded.flag1 != 0 || decoded.flag2 != 0 || decoded.flag3 != 0 {
        sink.record(Diagnostic::HeaderFlagsNonZero {
            protocol: ProtocolKind::DExtra,
            flag1: decoded.flag1,
            flag2: decoded.flag2,
            flag3: decoded.flag3,
        });
    }
    Ok((stream_id, decoded))
}

/// Verify DSVT magic at offset `[0..4]`.
fn check_dsvt_magic(bytes: &[u8]) -> Result<(), DExtraError> {
    let slice = bytes
        .get(..4)
        .ok_or(DExtraError::UnknownPacketLength { got: bytes.len() })?;
    if slice == DSVT_MAGIC.as_slice() {
        Ok(())
    } else {
        let mut got = [0u8; 4];
        got.copy_from_slice(slice);
        Err(DExtraError::DsvtMagicMissing { got })
    }
}

/// Extract the non-zero stream id from offsets `[12..14]` (little-endian).
fn extract_stream_id(bytes: &[u8]) -> Result<StreamId, DExtraError> {
    let lo = bytes.get(12).copied().unwrap_or(0);
    let hi = bytes.get(13).copied().unwrap_or(0);
    let raw = u16::from_le_bytes([lo, hi]);
    StreamId::new(raw).ok_or(DExtraError::StreamIdZero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::dextra::encode::{
        encode_connect_ack, encode_connect_link, encode_connect_nak, encode_poll, encode_unlink,
        encode_voice_data, encode_voice_eot, encode_voice_header,
    };
    use crate::types::Suffix;
    use crate::validator::NullSink;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const fn cs(bytes: [u8; 8]) -> Callsign {
        Callsign::from_wire_bytes(bytes)
    }

    #[expect(clippy::unwrap_used, reason = "compile-time validated: n != 0")]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
    }

    fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs(*b"XRF030 G"),
            rpt1: cs(*b"XRF030 C"),
            ur_call: cs(*b"CQCQCQ  "),
            my_call: cs(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    // ─── Client-side roundtrips ────────────────────────────────
    #[test]
    fn link_client_roundtrip() -> TestResult {
        // Encoder writes the callsign's first 7 bytes at [0..7],
        // leaves [7] as the memset space pad, and places the
        // client_module at [8]. The decoder reads [0..8] verbatim
        // (space at byte 7) and extracts the module from [8]
        // separately, so the round-trip is exact.
        let mut buf = [0u8; 16];
        let n = encode_connect_link(&mut buf, &cs(*b"W1AW    "), Module::C, Module::B)?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Link {
                callsign,
                reflector_module,
                client_module,
            } => {
                assert_eq!(callsign, cs(*b"W1AW    "));
                assert_eq!(reflector_module, Module::C);
                assert_eq!(client_module, Module::B);
            }
            other => return Err(format!("expected Link, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn unlink_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_unlink(&mut buf, &cs(*b"W1AW    "), Module::B)?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Unlink {
                callsign,
                client_module,
            } => {
                assert_eq!(callsign, cs(*b"W1AW    "));
                assert_eq!(client_module, Module::B);
            }
            other => return Err(format!("expected Unlink, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn poll_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf, &cs(*b"W1AW    "))?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Poll { callsign } => {
                assert_eq!(callsign, cs(*b"W1AW    "));
            }
            other => return Err(format!("expected Poll, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_header_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_header(&mut buf, sid(0xCAFE), &test_header())?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::VoiceHeader { stream_id, header } => {
                assert_eq!(stream_id, sid(0xCAFE));
                assert_eq!(header.my_call, test_header().my_call);
            }
            other => return Err(format!("expected VoiceHeader, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_data_client_roundtrip() -> TestResult {
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let mut buf = [0u8; 64];
        let n = encode_voice_data(&mut buf, sid(0x1234), 5, &frame)?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::VoiceData {
                stream_id,
                seq,
                frame: f,
            } => {
                assert_eq!(stream_id, sid(0x1234));
                assert_eq!(seq, 5);
                assert_eq!(f.ambe, [0x11; 9]);
                assert_eq!(f.slow_data, [0x22; 3]);
            }
            other => return Err(format!("expected VoiceData, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_eot_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_eot(&mut buf, sid(0x1234), 7)?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::VoiceEot { stream_id, seq } => {
                assert_eq!(stream_id, sid(0x1234));
                assert_eq!(seq & 0x40, 0x40, "EOT bit set");
                assert_eq!(seq & 0x3F, 7, "low bits preserve seq");
            }
            other => return Err(format!("expected VoiceEot, got {other:?}").into()),
        }
        Ok(())
    }

    // ─── Server-side roundtrips ────────────────────────────────
    #[test]
    fn connect_ack_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_connect_ack(&mut buf, &cs(*b"XRF030  "), Module::C)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::ConnectAck {
                callsign,
                reflector_module,
            } => {
                assert_eq!(callsign, cs(*b"XRF030  "));
                assert_eq!(reflector_module, Module::C);
            }
            other => return Err(format!("expected ConnectAck, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn connect_nak_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_connect_nak(&mut buf, &cs(*b"XRF030  "), Module::C)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::ConnectNak {
                callsign,
                reflector_module,
            } => {
                assert_eq!(callsign, cs(*b"XRF030  "));
                assert_eq!(reflector_module, Module::C);
            }
            other => return Err(format!("expected ConnectNak, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn poll_echo_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf, &cs(*b"XRF030  "))?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::PollEcho { callsign } => {
                assert_eq!(callsign, cs(*b"XRF030  "));
            }
            other => return Err(format!("expected PollEcho, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_header_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_header(&mut buf, sid(0xCAFE), &test_header())?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::VoiceHeader { stream_id, header } => {
                assert_eq!(stream_id, sid(0xCAFE));
                assert_eq!(header.my_call, test_header().my_call);
            }
            other => return Err(format!("expected VoiceHeader, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_data_server_roundtrip() -> TestResult {
        let frame = VoiceFrame {
            ambe: [0x33; 9],
            slow_data: [0x44; 3],
        };
        let mut buf = [0u8; 64];
        let n = encode_voice_data(&mut buf, sid(0x4321), 9, &frame)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::VoiceData {
                stream_id,
                seq,
                frame: f,
            } => {
                assert_eq!(stream_id, sid(0x4321));
                assert_eq!(seq, 9);
                assert_eq!(f.ambe, [0x33; 9]);
                assert_eq!(f.slow_data, [0x44; 3]);
            }
            other => return Err(format!("expected VoiceData, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_eot_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_eot(&mut buf, sid(0x1234), 7)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::VoiceEot { stream_id, seq } => {
                assert_eq!(stream_id, sid(0x1234));
                assert_eq!(seq & 0x40, 0x40, "EOT bit set");
                assert_eq!(seq & 0x3F, 7, "low bits preserve seq");
            }
            other => return Err(format!("expected VoiceEot, got {other:?}").into()),
        }
        Ok(())
    }

    // ─── Error cases ───────────────────────────────────────────
    #[test]
    fn unknown_length_returns_error() -> TestResult {
        let Err(err) = decode_client_to_server(&[0u8; 12], &mut NullSink) else {
            return Err("expected error for bad length".into());
        };
        assert!(matches!(err, DExtraError::UnknownPacketLength { got: 12 }));
        Ok(())
    }

    #[test]
    fn server_rejects_11_byte_client_link() -> TestResult {
        // 11-byte LINK is client-only; the server sends 14-byte replies.
        let Err(err) = decode_server_to_client(&[0u8; 11], &mut NullSink) else {
            return Err("expected error for server rejecting 11-byte".into());
        };
        assert!(matches!(err, DExtraError::UnknownPacketLength { got: 11 }));
        Ok(())
    }

    #[test]
    fn client_rejects_14_byte_server_reply() -> TestResult {
        // 14-byte ACK/NAK is server-only.
        let Err(err) = decode_client_to_server(&[0u8; 14], &mut NullSink) else {
            return Err("expected error for client rejecting 14-byte".into());
        };
        assert!(matches!(err, DExtraError::UnknownPacketLength { got: 14 }));
        Ok(())
    }

    #[test]
    fn link_with_invalid_client_module_byte() -> TestResult {
        // Valid callsign, invalid (lowercase) client module byte.
        let mut bytes = [b' '; 11];
        bytes[..4].copy_from_slice(b"W1AW");
        bytes[8] = b'b'; // lowercase — invalid
        bytes[9] = b'C';
        bytes[10] = 0x00;
        let Err(err) = decode_client_to_server(&bytes, &mut NullSink) else {
            return Err("expected error for invalid module byte".into());
        };
        assert!(matches!(
            err,
            DExtraError::InvalidModuleByte {
                offset: 8,
                byte: b'b'
            }
        ));
        Ok(())
    }

    #[test]
    fn link_with_invalid_reflector_module_byte() -> TestResult {
        let mut bytes = [b' '; 11];
        bytes[..4].copy_from_slice(b"W1AW");
        bytes[8] = b'B';
        bytes[9] = b'1'; // digit — invalid
        bytes[10] = 0x00;
        let Err(err) = decode_client_to_server(&bytes, &mut NullSink) else {
            return Err("expected error for invalid reflector module byte".into());
        };
        assert!(matches!(
            err,
            DExtraError::InvalidModuleByte {
                offset: 9,
                byte: b'1'
            }
        ));
        Ok(())
    }

    #[test]
    fn voice_data_with_zero_stream_id_rejected() -> TestResult {
        let mut bytes = [0u8; 27];
        bytes[..4].copy_from_slice(b"DSVT");
        bytes[4] = 0x20;
        bytes[8] = 0x20;
        // stream_id at [12..14] left as zero.
        let Err(err) = decode_client_to_server(&bytes, &mut NullSink) else {
            return Err("expected error for zero stream id".into());
        };
        assert!(matches!(err, DExtraError::StreamIdZero));
        Ok(())
    }

    #[test]
    fn voice_header_missing_dsvt_magic_rejected() -> TestResult {
        let mut bytes = [0u8; 56];
        bytes[..4].copy_from_slice(b"XXXX"); // wrong magic
        bytes[4] = 0x10;
        bytes[12] = 0x34;
        bytes[13] = 0x12;
        let Err(err) = decode_client_to_server(&bytes, &mut NullSink) else {
            return Err("expected error for bad DSVT magic".into());
        };
        assert!(matches!(err, DExtraError::DsvtMagicMissing { .. }));
        Ok(())
    }

    #[test]
    fn connect_reply_with_unknown_tag() -> TestResult {
        let mut bytes = [b' '; 14];
        bytes[..4].copy_from_slice(b"XRF0");
        bytes[8] = b'C';
        bytes[9] = b'C';
        // Tag is at [10..13] per the reference; byte 13 is NUL.
        bytes[10..13].copy_from_slice(b"FOO");
        bytes[13] = 0x00;
        let Err(err) = decode_server_to_client(&bytes, &mut NullSink) else {
            return Err("expected error for unknown connect tag".into());
        };
        assert!(matches!(err, DExtraError::UnknownConnectTag { .. }));
        Ok(())
    }
}
