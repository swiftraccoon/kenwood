//! `DCS` packet decoders.
//!
//! Both directions — `decode_client_to_server` parses packets a
//! client would send, `decode_server_to_client` parses packets a
//! reflector would send.
//!
//! Lenient: recoverable malformations push `Diagnostic`s to the
//! supplied sink but still return a parsed packet. Only fatal errors
//! (wrong length, missing magic, zero stream id, invalid module byte)
//! return `Err`.

use crate::header::DStarHeader;
use crate::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
use crate::validator::{Diagnostic, DiagnosticSink};
use crate::voice::VoiceFrame;

use super::consts::{
    CONNECT_ACK_TAG, CONNECT_NAK_TAG, CONNECT_REPLY_LEN, LINK_LEN, POLL_LEN, UNLINK_LEN, VOICE_LEN,
    VOICE_MAGIC,
};
use super::error::DcsError;
use super::packet::{ClientPacket, GatewayType, ServerPacket};

/// Decode a UDP datagram sent from a `DCS` client (client → server).
///
/// # Errors
///
/// - [`DcsError::UnknownPacketLength`] for unrecognized lengths
/// - [`DcsError::VoiceMagicMissing`] for 100-byte packets without `b"0001"` magic
/// - [`DcsError::StreamIdZero`] for voice packets with stream id 0
/// - [`DcsError::InvalidModuleByte`] for LINK with a non-A-Z module byte
///   at `[8]` or `[9]`
/// - [`DcsError::UnlinkModuleByteInvalid`] for UNLINK with byte `[9]` ≠
///   `0x20`
///
/// # See also
///
/// `ircDDBGateway/Common/DCSProtocolHandler.cpp` — the reference
/// parser this decoder mirrors. `xlxd/src/cdcsprotocol.cpp` is
/// a mirror reference.
pub fn decode_client_to_server(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DcsError> {
    let len = bytes.len();
    match len {
        POLL_LEN => Ok(decode_client_poll(bytes)),
        UNLINK_LEN => decode_client_unlink(bytes),
        LINK_LEN => decode_client_link(bytes),
        VOICE_LEN => decode_client_voice(bytes, sink),
        _ => Err(DcsError::UnknownPacketLength { got: len }),
    }
}

/// Decode a UDP datagram sent from a `DCS` reflector (server → client).
///
/// # Errors
///
/// Same kinds as [`decode_client_to_server`], but produces
/// [`ServerPacket`] variants. 14-byte ACK/NAK and 17-byte poll replies
/// are server-side packets; 100-byte voice frames are forwarded
/// bidirectionally.
///
/// # See also
///
/// `ircDDBGateway/Common/DCSProtocolHandler.cpp` — the reference
/// parser for the server side of the `DCS` wire format.
pub fn decode_server_to_client(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DcsError> {
    let len = bytes.len();
    match len {
        POLL_LEN => Ok(decode_server_poll(bytes)),
        CONNECT_REPLY_LEN => decode_server_connect_reply(bytes),
        VOICE_LEN => decode_server_voice(bytes, sink),
        _ => Err(DcsError::UnknownPacketLength { got: len }),
    }
}

/// Extract an 8-byte callsign from a byte range, substituting spaces
/// for embedded zero bytes (so the wire representation stays clean
/// through `Callsign::from_wire_bytes`).
///
/// This reader is used for every DCS callsign slot — poll,
/// reflector-callsign at `[11..19]`, and the connect-packet
/// prefix at `[0..8]`. The wire format has byte `[7]` as the
/// `memset` pad slot and byte `[8]` outside the 8-byte window
/// holding the module letter (for LINK/ACK/NAK) or `0x00` (for
/// poll). Our Rust API exposes the module as a separate `Module`
/// field, so this reader deliberately does NOT splice byte `[8]`
/// into the callsign. Keeping byte `[7]` as the plain space from
/// the wire matches what the 17-byte poll decoder sees and lets
/// the server session compare stored callsigns against incoming
/// polls byte-for-byte.
fn extract_callsign(src: &[u8]) -> Callsign {
    let mut buf = [b' '; 8];
    let take = src.len().min(8);
    if let Some(dst) = buf.get_mut(..take)
        && let Some(s) = src.get(..take)
    {
        dst.copy_from_slice(s);
    }
    for b in &mut buf {
        if *b == 0 {
            *b = b' ';
        }
    }
    Callsign::from_wire_bytes(buf)
}

/// Decode a 519-byte LINK packet from the client side.
fn decode_client_link(bytes: &[u8]) -> Result<ClientPacket, DcsError> {
    let callsign = extract_callsign(bytes.get(..8).unwrap_or(&[]));
    let client_byte = bytes.get(8).copied().unwrap_or(0);
    let client_module =
        Module::try_from_byte(client_byte).map_err(|_| DcsError::InvalidModuleByte {
            offset: 8,
            byte: client_byte,
        })?;
    let reflector_byte = bytes.get(9).copied().unwrap_or(0);
    let reflector_module =
        Module::try_from_byte(reflector_byte).map_err(|_| DcsError::InvalidModuleByte {
            offset: 9,
            byte: reflector_byte,
        })?;
    let reflector_callsign = extract_callsign(bytes.get(11..19).unwrap_or(&[]));
    // We don't parse the HTML payload — default to Repeater.
    Ok(ClientPacket::Link {
        callsign,
        client_module,
        reflector_module,
        reflector_callsign,
        gateway_type: GatewayType::Repeater,
    })
}

/// Decode a 19-byte UNLINK packet from the client side.
fn decode_client_unlink(bytes: &[u8]) -> Result<ClientPacket, DcsError> {
    let callsign = extract_callsign(bytes.get(..8).unwrap_or(&[]));
    let client_byte = bytes.get(8).copied().unwrap_or(0);
    let client_module =
        Module::try_from_byte(client_byte).map_err(|_| DcsError::InvalidModuleByte {
            offset: 8,
            byte: client_byte,
        })?;
    let marker = bytes.get(9).copied().unwrap_or(0);
    if marker != b' ' {
        return Err(DcsError::UnlinkModuleByteInvalid { byte: marker });
    }
    let reflector_callsign = extract_callsign(bytes.get(11..19).unwrap_or(&[]));
    Ok(ClientPacket::Unlink {
        callsign,
        client_module,
        reflector_callsign,
    })
}

/// Decode a 17-byte poll packet from the client side.
fn decode_client_poll(bytes: &[u8]) -> ClientPacket {
    let callsign = extract_callsign(bytes.get(..8).unwrap_or(&[]));
    let reflector_callsign = extract_callsign(bytes.get(9..17).unwrap_or(&[]));
    ClientPacket::Poll {
        callsign,
        reflector_callsign,
    }
}

/// Decode a 17-byte poll echo from the server side.
fn decode_server_poll(bytes: &[u8]) -> ServerPacket {
    let callsign = extract_callsign(bytes.get(..8).unwrap_or(&[]));
    let reflector_callsign = extract_callsign(bytes.get(9..17).unwrap_or(&[]));
    ServerPacket::PollEcho {
        callsign,
        reflector_callsign,
    }
}

/// Decode a 14-byte ACK or NAK reply from the server side.
fn decode_server_connect_reply(bytes: &[u8]) -> Result<ServerPacket, DcsError> {
    let callsign = extract_callsign(bytes.get(..8).unwrap_or(&[]));
    let module_byte = bytes.get(9).copied().unwrap_or(0);
    let reflector_module =
        Module::try_from_byte(module_byte).map_err(|_| DcsError::InvalidModuleByte {
            offset: 9,
            byte: module_byte,
        })?;
    // Tag is at [10..13], NUL at [13] per
    // `ircDDBGateway/Common/ConnectData.cpp:374-393`.
    let mut tag = [0u8; 3];
    if let Some(src) = bytes.get(10..13) {
        tag.copy_from_slice(src);
    }
    if tag == CONNECT_ACK_TAG {
        Ok(ServerPacket::ConnectAck {
            callsign,
            reflector_module,
        })
    } else if tag == CONNECT_NAK_TAG {
        Ok(ServerPacket::ConnectNak {
            callsign,
            reflector_module,
        })
    } else {
        Err(DcsError::UnknownConnectTag { tag })
    }
}

/// Decode a 100-byte voice packet from the client side.
fn decode_client_voice(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DcsError> {
    let (header, stream_id, seq, frame, is_end) = parse_voice(bytes, sink)?;
    Ok(ClientPacket::Voice {
        header,
        stream_id,
        seq,
        frame,
        is_end,
    })
}

/// Decode a 100-byte voice packet from the server side.
fn decode_server_voice(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DcsError> {
    let (header, stream_id, seq, frame, is_end) = parse_voice(bytes, sink)?;
    Ok(ServerPacket::Voice {
        header,
        stream_id,
        seq,
        frame,
        is_end,
    })
}

/// Shared 100-byte voice parser. Returns the embedded header, stream
/// id, seq byte (with `0x40` stripped), the AMBE + slow data frame,
/// and `is_end` flag.
fn parse_voice(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<(DStarHeader, StreamId, u8, VoiceFrame, bool), DcsError> {
    // Magic check at [0..4].
    let magic = bytes
        .get(..4)
        .ok_or(DcsError::UnknownPacketLength { got: bytes.len() })?;
    if magic != VOICE_MAGIC.as_slice() {
        let mut got = [0u8; 4];
        got.copy_from_slice(magic);
        return Err(DcsError::VoiceMagicMissing { got });
    }

    // Stream id at [43..45] little-endian.
    let lo = bytes.get(43).copied().unwrap_or(0);
    let hi = bytes.get(44).copied().unwrap_or(0);
    let raw = u16::from_le_bytes([lo, hi]);
    let stream_id = StreamId::new(raw).ok_or(DcsError::StreamIdZero)?;

    // Seq at [45]. Strip 0x40 bit to get the "real" seq and use it for
    // is_end detection. We also consult bytes [55..58] for the EOT
    // marker (the reference DCS uses the marker; xlxd additionally
    // sets the 0x40 bit). Either signal flags end-of-stream.
    let seq_raw = bytes.get(45).copied().unwrap_or(0);
    let eot_bit = (seq_raw & 0x40) != 0;
    let seq = seq_raw & 0x3F;

    // AMBE at [46..55].
    let mut ambe = [0u8; 9];
    if let Some(src) = bytes.get(46..55) {
        ambe.copy_from_slice(src);
    }
    // Slow data at [55..58].
    let mut slow = [0u8; 3];
    if let Some(src) = bytes.get(55..58) {
        slow.copy_from_slice(src);
    }
    let eot_marker = slow == [0x55, 0x55, 0x55];
    let is_end = eot_bit || eot_marker;

    let frame = VoiceFrame {
        ambe,
        slow_data: slow,
    };

    // Embedded header at [4..43] per HeaderData.cpp:520-528.
    let header = decode_dcs_header_from_voice(bytes);
    if header.flag1 != 0 || header.flag2 != 0 || header.flag3 != 0 {
        sink.record(Diagnostic::HeaderFlagsNonZero {
            protocol: ProtocolKind::Dcs,
            flag1: header.flag1,
            flag2: header.flag2,
            flag3: header.flag3,
        });
    }

    Ok((header, stream_id, seq, frame, is_end))
}

/// Extract a `DStarHeader` from the embedded `[4..43]` region of a
/// DCS voice packet.
///
/// DCS stores the header fields starting at offset 4 with a layout
/// that differs from [`DStarHeader::encode`]'s default 41-byte
/// encoding — the flag bytes are at offsets 4/5/6 and the suffix is
/// at offsets 39..43 (no CRC). Build the struct manually from the
/// field positions.
fn decode_dcs_header_from_voice(bytes: &[u8]) -> DStarHeader {
    let flag1 = bytes.get(4).copied().unwrap_or(0);
    let flag2 = bytes.get(5).copied().unwrap_or(0);
    let flag3 = bytes.get(6).copied().unwrap_or(0);

    let mut rpt2 = [b' '; 8];
    if let Some(src) = bytes.get(7..15) {
        rpt2.copy_from_slice(src);
    }
    let mut rpt1 = [b' '; 8];
    if let Some(src) = bytes.get(15..23) {
        rpt1.copy_from_slice(src);
    }
    let mut ur = [b' '; 8];
    if let Some(src) = bytes.get(23..31) {
        ur.copy_from_slice(src);
    }
    let mut my = [b' '; 8];
    if let Some(src) = bytes.get(31..39) {
        my.copy_from_slice(src);
    }
    let mut sfx = [b' '; 4];
    if let Some(src) = bytes.get(39..43) {
        sfx.copy_from_slice(src);
    }

    DStarHeader {
        flag1,
        flag2,
        flag3,
        rpt2: Callsign::from_wire_bytes(rpt2),
        rpt1: Callsign::from_wire_bytes(rpt1),
        ur_call: Callsign::from_wire_bytes(ur),
        my_call: Callsign::from_wire_bytes(my),
        my_suffix: Suffix::from_wire_bytes(sfx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::dcs::encode::{
        encode_connect_ack, encode_connect_link, encode_connect_nak, encode_connect_unlink,
        encode_poll_reply, encode_poll_request, encode_voice,
    };
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
            rpt2: cs(*b"DCS001 G"),
            rpt1: cs(*b"DCS001 C"),
            ur_call: cs(*b"CQCQCQ  "),
            my_call: cs(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    // ─── Client roundtrips ──────────────────────────────────
    #[test]
    fn link_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 600];
        let n = encode_connect_link(
            &mut buf,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Repeater,
        )?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Link {
                callsign,
                client_module,
                reflector_module,
                reflector_callsign,
                gateway_type,
            } => {
                assert_eq!(callsign, cs(*b"W1AW    "));
                assert_eq!(client_module, Module::B);
                assert_eq!(reflector_module, Module::C);
                assert_eq!(reflector_callsign, cs(*b"DCS001  "));
                assert_eq!(gateway_type, GatewayType::Repeater);
            }
            other => return Err(format!("expected Link, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn unlink_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_connect_unlink(&mut buf, &cs(*b"W1AW    "), Module::B, &cs(*b"DCS001  "))?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Unlink {
                callsign,
                client_module,
                reflector_callsign,
            } => {
                assert_eq!(callsign, cs(*b"W1AW    "));
                assert_eq!(client_module, Module::B);
                assert_eq!(reflector_callsign, cs(*b"DCS001  "));
            }
            other => return Err(format!("expected Unlink, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn poll_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_poll_request(&mut buf, &cs(*b"W1AW    "), &cs(*b"DCS001  "))?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Poll {
                callsign,
                reflector_callsign,
            } => {
                assert_eq!(callsign, cs(*b"W1AW    "));
                assert_eq!(reflector_callsign, cs(*b"DCS001  "));
            }
            other => return Err(format!("expected Poll, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let n = encode_voice(&mut buf, &test_header(), sid(0xCAFE), 5, &frame, false)?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Voice {
                header,
                stream_id,
                seq,
                frame: f,
                is_end,
            } => {
                assert_eq!(stream_id, sid(0xCAFE));
                assert_eq!(seq, 5);
                assert_eq!(f.ambe, [0x11; 9]);
                assert_eq!(f.slow_data, [0x22; 3]);
                assert!(!is_end);
                assert_eq!(header.my_call, test_header().my_call);
                assert_eq!(header.rpt2, test_header().rpt2);
                assert_eq!(header.ur_call, test_header().ur_call);
            }
            other => return Err(format!("expected Voice, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_eot_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let n = encode_voice(&mut buf, &test_header(), sid(0x1234), 7, &frame, true)?;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ClientPacket::Voice {
                stream_id,
                seq,
                is_end,
                ..
            } => {
                assert_eq!(stream_id, sid(0x1234));
                assert_eq!(seq, 7);
                assert!(is_end, "is_end should be true");
            }
            other => return Err(format!("expected Voice, got {other:?}").into()),
        }
        Ok(())
    }

    // ─── Server roundtrips ─────────────────────────────────
    #[test]
    fn connect_ack_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_connect_ack(&mut buf, &cs(*b"DCS001  "), Module::C)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::ConnectAck {
                callsign,
                reflector_module,
            } => {
                assert_eq!(callsign, cs(*b"DCS001  "));
                assert_eq!(reflector_module, Module::C);
            }
            other => return Err(format!("expected ConnectAck, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn connect_nak_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_connect_nak(&mut buf, &cs(*b"DCS001  "), Module::C)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::ConnectNak {
                callsign,
                reflector_module,
            } => {
                assert_eq!(callsign, cs(*b"DCS001  "));
                assert_eq!(reflector_module, Module::C);
            }
            other => return Err(format!("expected ConnectNak, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn poll_echo_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 32];
        let n = encode_poll_reply(&mut buf, &cs(*b"DCS001  "), &cs(*b"DCS001  "))?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::PollEcho {
                callsign,
                reflector_callsign,
            } => {
                assert_eq!(callsign, cs(*b"DCS001  "));
                assert_eq!(reflector_callsign, cs(*b"DCS001  "));
            }
            other => return Err(format!("expected PollEcho, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0x33; 9],
            slow_data: [0x44; 3],
        };
        let n = encode_voice(&mut buf, &test_header(), sid(0x4321), 9, &frame, false)?;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut NullSink)?;
        match pkt {
            ServerPacket::Voice {
                stream_id,
                seq,
                frame: f,
                is_end,
                ..
            } => {
                assert_eq!(stream_id, sid(0x4321));
                assert_eq!(seq, 9);
                assert_eq!(f.ambe, [0x33; 9]);
                assert_eq!(f.slow_data, [0x44; 3]);
                assert!(!is_end);
            }
            other => return Err(format!("expected Voice, got {other:?}").into()),
        }
        Ok(())
    }

    // ─── Error cases ────────────────────────────────────────
    #[test]
    fn unknown_length_returns_error() -> TestResult {
        let Err(err) = decode_client_to_server(&[0u8; 12], &mut NullSink) else {
            return Err("expected error for bad length".into());
        };
        assert!(matches!(err, DcsError::UnknownPacketLength { got: 12 }));
        Ok(())
    }

    #[test]
    fn server_rejects_19_byte_client_unlink() -> TestResult {
        // 19-byte UNLINK is client-only; server never sends these.
        let Err(err) = decode_server_to_client(&[0u8; 19], &mut NullSink) else {
            return Err("expected error for server rejecting 19-byte".into());
        };
        assert!(matches!(err, DcsError::UnknownPacketLength { got: 19 }));
        Ok(())
    }

    #[test]
    fn client_rejects_14_byte_server_reply() -> TestResult {
        // 14-byte ACK/NAK is server-only.
        let Err(err) = decode_client_to_server(&[0u8; 14], &mut NullSink) else {
            return Err("expected error for client rejecting 14-byte".into());
        };
        assert!(matches!(err, DcsError::UnknownPacketLength { got: 14 }));
        Ok(())
    }

    #[test]
    fn link_with_invalid_client_module_byte() -> TestResult {
        // Start with a valid LINK then stomp the client module byte.
        let mut buf = [0u8; 600];
        let _n = encode_connect_link(
            &mut buf,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Repeater,
        )?;
        buf[8] = b'b'; // lowercase — invalid
        let Err(err) = decode_client_to_server(&buf[..LINK_LEN], &mut NullSink) else {
            return Err("expected error for invalid module byte".into());
        };
        assert!(matches!(
            err,
            DcsError::InvalidModuleByte {
                offset: 8,
                byte: b'b'
            }
        ));
        Ok(())
    }

    #[test]
    fn link_with_invalid_reflector_module_byte() -> TestResult {
        let mut buf = [0u8; 600];
        let _n = encode_connect_link(
            &mut buf,
            &cs(*b"W1AW    "),
            Module::B,
            Module::C,
            &cs(*b"DCS001  "),
            GatewayType::Repeater,
        )?;
        buf[9] = b'1'; // digit — invalid
        let Err(err) = decode_client_to_server(&buf[..LINK_LEN], &mut NullSink) else {
            return Err("expected error for invalid reflector module byte".into());
        };
        assert!(matches!(
            err,
            DcsError::InvalidModuleByte {
                offset: 9,
                byte: b'1'
            }
        ));
        Ok(())
    }

    #[test]
    fn unlink_with_non_space_at_position_9() -> TestResult {
        let mut buf = [0u8; 32];
        let _n = encode_connect_unlink(&mut buf, &cs(*b"W1AW    "), Module::B, &cs(*b"DCS001  "))?;
        buf[9] = b'C'; // not space
        let Err(err) = decode_client_to_server(&buf[..UNLINK_LEN], &mut NullSink) else {
            return Err("expected error for non-space marker at position 9".into());
        };
        assert!(matches!(
            err,
            DcsError::UnlinkModuleByteInvalid { byte: b'C' }
        ));
        Ok(())
    }

    #[test]
    fn voice_with_zero_stream_id_rejected() -> TestResult {
        let mut buf = [0u8; 100];
        buf[..4].copy_from_slice(b"0001");
        // stream id at [43..45] left as zero
        let Err(err) = decode_client_to_server(&buf, &mut NullSink) else {
            return Err("expected error for zero stream id".into());
        };
        assert!(matches!(err, DcsError::StreamIdZero));
        Ok(())
    }

    #[test]
    fn voice_missing_magic_rejected() -> TestResult {
        let mut buf = [0u8; 100];
        buf[..4].copy_from_slice(b"XXXX"); // wrong magic
        buf[43] = 0x34;
        buf[44] = 0x12;
        let Err(err) = decode_client_to_server(&buf, &mut NullSink) else {
            return Err("expected error for bad voice magic".into());
        };
        assert!(matches!(err, DcsError::VoiceMagicMissing { .. }));
        Ok(())
    }

    #[test]
    fn connect_reply_with_unknown_tag() -> TestResult {
        let mut buf = [0u8; 14];
        buf[..8].copy_from_slice(b"DCS001  ");
        buf[8] = b'C';
        buf[9] = b'C';
        // Tag is at [10..13], NUL at [13] per the reference.
        buf[10..13].copy_from_slice(b"FOO");
        buf[13] = 0x00;
        let Err(err) = decode_server_to_client(&buf, &mut NullSink) else {
            return Err("expected error for unknown connect tag".into());
        };
        assert!(matches!(err, DcsError::UnknownConnectTag { .. }));
        Ok(())
    }

    #[test]
    fn voice_eot_marker_alone_also_detected() -> TestResult {
        // A packet that carries the 0x55 marker but NOT the 0x40 seq
        // bit should still parse as EOT (the reference DCS writes the
        // marker, xlxd adds the bit).
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0; 3],
        };
        let _n = encode_voice(&mut buf, &test_header(), sid(0x1234), 3, &frame, false)?;
        // Manually set the EOT marker without touching the seq byte.
        buf[55] = 0x55;
        buf[56] = 0x55;
        buf[57] = 0x55;
        let pkt = decode_client_to_server(
            buf.get(..VOICE_LEN).ok_or("VOICE_LEN within buf")?,
            &mut NullSink,
        )?;
        match pkt {
            ClientPacket::Voice { is_end, seq, .. } => {
                assert_eq!(seq, 3);
                assert!(is_end, "EOT marker alone should flag is_end");
            }
            other => return Err(format!("expected Voice, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn voice_flag_bytes_non_zero_raises_diagnostic() -> TestResult {
        use crate::validator::VecSink;

        let header = DStarHeader {
            flag1: 0xAA,
            ..test_header()
        };
        let mut buf = [0u8; 128];
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let _n = encode_voice(&mut buf, &header, sid(1), 0, &frame, false)?;
        let mut sink = VecSink::default();
        let pkt = decode_client_to_server(
            buf.get(..VOICE_LEN).ok_or("VOICE_LEN within buf")?,
            &mut sink,
        )?;
        assert!(matches!(pkt, ClientPacket::Voice { .. }));
        assert_eq!(sink.len(), 1, "expected one diagnostic");
        Ok(())
    }
}
