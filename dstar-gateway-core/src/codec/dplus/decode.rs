//! `DPlus` packet decoders.
//!
//! Both directions — `decode_client_to_server` parses packets a
//! client would send, `decode_server_to_client` parses packets a
//! reflector would send.
//!
//! Lenient: recoverable malformations push `Diagnostic`s to the
//! supplied sink but still return a parsed packet. Only fatal errors
//! (wrong length, missing magic, zero stream id) return `Err`.

use crate::header::{DStarHeader, ENCODED_LEN};
use crate::types::{Callsign, ProtocolKind, StreamId};
use crate::validator::{Diagnostic, DiagnosticSink};
use crate::voice::VoiceFrame;

use super::consts::{DSVT_MAGIC, LINK2_ACCEPT_TAG, LINK2_BUSY_TAG};
use super::error::DPlusError;
use super::packet::{ClientPacket, Link2Result, ServerPacket};

/// Decode a UDP datagram sent from a `DPlus` reflector (server → client).
///
/// # Errors
///
/// - `DPlusError::UnknownPacketLength` for unrecognized lengths
/// - `DPlusError::DsvtMagicMissing` for DSVT-length packets without the magic
/// - `DPlusError::StreamIdZero` for voice packets with stream id 0
/// - `DPlusError::InvalidShortControlByte` for 5-byte packets with unknown control
///
/// # See also
///
/// `ircDDBGateway/Common/DPlusProtocolHandler.cpp` — the reference
/// parser this decoder mirrors (length-dispatch then DSVT-magic
/// branch). `xlxd/src/cdplusprotocol.cpp` is a mirror reference.
pub fn decode_server_to_client(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DPlusError> {
    let len = bytes.len();
    let is_dsvt = len >= 6 && bytes.get(2..6) == Some(DSVT_MAGIC.as_slice());

    if !is_dsvt {
        return match len {
            3 => Ok(ServerPacket::PollEcho),
            5 => {
                let ctrl = bytes.get(4).copied().unwrap_or(0);
                match ctrl {
                    0x01 => Ok(ServerPacket::Link1Ack),
                    0x00 => Ok(ServerPacket::UnlinkAck),
                    other => Err(DPlusError::InvalidShortControlByte { byte: other }),
                }
            }
            8 => {
                let tag_slice = bytes.get(4..8).unwrap_or(&[]);
                let mut tag = [0u8; 4];
                tag.copy_from_slice(tag_slice);
                let result = if tag == LINK2_ACCEPT_TAG {
                    Link2Result::Accept
                } else if tag == LINK2_BUSY_TAG {
                    Link2Result::Busy
                } else {
                    sink.record(Diagnostic::UnknownLink2Reply { reply: tag });
                    Link2Result::Unknown { reply: tag }
                };
                Ok(ServerPacket::Link2Reply { result })
            }
            28 => {
                let cs_bytes = bytes.get(4..12).unwrap_or(&[]);
                let mut cs = [b' '; 8];
                let copy_len = cs_bytes.len().min(8);
                if let Some(dst) = cs.get_mut(..copy_len)
                    && let Some(src) = cs_bytes.get(..copy_len)
                {
                    dst.copy_from_slice(src);
                }
                // Treat trailing zeros as padding — replace them with spaces so
                // `Callsign::from_wire_bytes` stores a clean wire representation.
                for b in &mut cs {
                    if *b == 0 {
                        *b = b' ';
                    }
                }
                Ok(ServerPacket::Link2Echo {
                    callsign: Callsign::from_wire_bytes(cs),
                })
            }
            _ => Err(DPlusError::UnknownPacketLength { got: len }),
        };
    }

    // DSVT-framed path.
    decode_dsvt_server(bytes, sink)
}

/// Decode a UDP datagram sent from a `DPlus` client (client → server).
///
/// # Errors
///
/// Same as [`decode_server_to_client`], but produces [`ClientPacket`]
/// variants. The 8-byte length is NOT accepted here — clients do not
/// send 8-byte LINK2 replies.
///
/// # See also
///
/// `ircDDBGateway/Common/DPlusProtocolHandler.cpp` — mirror parser
/// on the server side.
pub fn decode_client_to_server(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DPlusError> {
    let len = bytes.len();
    let is_dsvt = len >= 6 && bytes.get(2..6) == Some(DSVT_MAGIC.as_slice());

    if !is_dsvt {
        return match len {
            3 => Ok(ClientPacket::Poll),
            5 => {
                let ctrl = bytes.get(4).copied().unwrap_or(0);
                match ctrl {
                    0x01 => Ok(ClientPacket::Link1),
                    0x00 => Ok(ClientPacket::Unlink),
                    other => Err(DPlusError::InvalidShortControlByte { byte: other }),
                }
            }
            28 => {
                let cs_bytes = bytes.get(4..12).unwrap_or(&[]);
                let mut cs = [b' '; 8];
                let copy_len = cs_bytes.len().min(8);
                if let Some(dst) = cs.get_mut(..copy_len)
                    && let Some(src) = cs_bytes.get(..copy_len)
                {
                    dst.copy_from_slice(src);
                }
                for b in &mut cs {
                    if *b == 0 {
                        *b = b' ';
                    }
                }
                Ok(ClientPacket::Link2 {
                    callsign: Callsign::from_wire_bytes(cs),
                })
            }
            _ => Err(DPlusError::UnknownPacketLength { got: len }),
        };
    }

    // DSVT-framed path.
    decode_dsvt_client(bytes, sink)
}

fn decode_dsvt_server(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ServerPacket, DPlusError> {
    let (stream_id, header, frame_bytes, is_header, is_eot, len) = parse_dsvt_common(bytes, sink)?;
    if is_header {
        let Some(hdr) = header else {
            return Err(DPlusError::UnknownPacketLength { got: len });
        };
        Ok(ServerPacket::VoiceHeader {
            stream_id,
            header: hdr,
        })
    } else if is_eot {
        let seq = bytes.get(16).copied().unwrap_or(0);
        Ok(ServerPacket::VoiceEot { stream_id, seq })
    } else {
        let Some(frame) = frame_bytes else {
            return Err(DPlusError::UnknownPacketLength { got: len });
        };
        let seq = bytes.get(16).copied().unwrap_or(0);
        Ok(ServerPacket::VoiceData {
            stream_id,
            seq,
            frame,
        })
    }
}

fn decode_dsvt_client(
    bytes: &[u8],
    sink: &mut dyn DiagnosticSink,
) -> Result<ClientPacket, DPlusError> {
    let (stream_id, header, frame_bytes, is_header, is_eot, len) = parse_dsvt_common(bytes, sink)?;
    if is_header {
        let Some(hdr) = header else {
            return Err(DPlusError::UnknownPacketLength { got: len });
        };
        Ok(ClientPacket::VoiceHeader {
            stream_id,
            header: hdr,
        })
    } else if is_eot {
        let seq = bytes.get(16).copied().unwrap_or(0);
        Ok(ClientPacket::VoiceEot { stream_id, seq })
    } else {
        let Some(frame) = frame_bytes else {
            return Err(DPlusError::UnknownPacketLength { got: len });
        };
        let seq = bytes.get(16).copied().unwrap_or(0);
        Ok(ClientPacket::VoiceData {
            stream_id,
            seq,
            frame,
        })
    }
}

type DsvtParse = (
    StreamId,
    Option<DStarHeader>,
    Option<VoiceFrame>,
    bool,
    bool,
    usize,
);

fn parse_dsvt_common(bytes: &[u8], sink: &mut dyn DiagnosticSink) -> Result<DsvtParse, DPlusError> {
    let len = bytes.len();
    match len {
        58 | 29 | 32 => {}
        _ => return Err(DPlusError::UnknownPacketLength { got: len }),
    }

    // Stream id at [14..16] little-endian.
    let lo = bytes.get(14).copied().unwrap_or(0);
    let hi = bytes.get(15).copied().unwrap_or(0);
    let raw_sid = u16::from_le_bytes([lo, hi]);
    let stream_id = StreamId::new(raw_sid).ok_or(DPlusError::StreamIdZero)?;

    let is_header = len == 58 && bytes.get(6).copied() == Some(0x10);
    let is_eot = len == 32 && bytes.get(6).copied() == Some(0x20);
    let is_voice = len == 29 && bytes.get(6).copied() == Some(0x20);

    if !is_header && !is_eot && !is_voice {
        return Err(DPlusError::UnknownPacketLength { got: len });
    }

    let header = if is_header {
        let hdr_slice = bytes
            .get(17..58)
            .ok_or(DPlusError::UnknownPacketLength { got: len })?;
        let mut arr = [0u8; ENCODED_LEN];
        arr.copy_from_slice(hdr_slice);
        let decoded = DStarHeader::decode(&arr);
        // Lenient: diagnose non-zero flag bytes but still return the header.
        if decoded.flag1 != 0 || decoded.flag2 != 0 || decoded.flag3 != 0 {
            sink.record(Diagnostic::HeaderFlagsNonZero {
                protocol: ProtocolKind::DPlus,
                flag1: decoded.flag1,
                flag2: decoded.flag2,
                flag3: decoded.flag3,
            });
        }
        Some(decoded)
    } else {
        None
    };

    let frame = if is_voice {
        let mut ambe = [0u8; 9];
        let mut slow = [0u8; 3];
        if let Some(src) = bytes.get(17..26) {
            ambe.copy_from_slice(src);
        }
        if let Some(src) = bytes.get(26..29) {
            slow.copy_from_slice(src);
        }
        Some(VoiceFrame {
            ambe,
            slow_data: slow,
        })
    } else {
        None
    };

    Ok((stream_id, header, frame, is_header, is_eot, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::dplus::encode::{
        encode_link1, encode_link2, encode_link2_reply, encode_poll, encode_unlink,
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
            rpt2: cs(*b"REF030 G"),
            rpt1: cs(*b"REF030 C"),
            ur_call: cs(*b"CQCQCQ  "),
            my_call: cs(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    // ─── Client-side (what client sends) roundtrips ───────────
    #[test]
    fn link1_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link1(&mut buf)?;
        let mut sink = NullSink;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(pkt, ClientPacket::Link1));
        Ok(())
    }

    #[test]
    fn unlink_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_unlink(&mut buf)?;
        let mut sink = NullSink;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(pkt, ClientPacket::Unlink));
        Ok(())
    }

    #[test]
    fn poll_client_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf)?;
        let mut sink = NullSink;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(pkt, ClientPacket::Poll));
        Ok(())
    }

    #[test]
    fn link2_client_roundtrip() -> TestResult {
        let cs_in = cs(*b"W1AW    ");
        let mut buf = [0u8; 32];
        let n = encode_link2(&mut buf, &cs_in)?;
        let mut sink = NullSink;
        let pkt = decode_client_to_server(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        match pkt {
            ClientPacket::Link2 { callsign } => assert_eq!(callsign, cs_in),
            other => return Err(format!("expected Link2, got {other:?}").into()),
        }
        Ok(())
    }

    // ─── Server-side (what server sends) roundtrips ───────────
    #[test]
    fn link1_ack_server_roundtrip() -> TestResult {
        // LINK1 ACK uses the same bytes as LINK1 (the server echoes it).
        let mut buf = [0u8; 16];
        let n = encode_link1(&mut buf)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(pkt, ServerPacket::Link1Ack));
        Ok(())
    }

    #[test]
    fn unlink_ack_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_unlink(&mut buf)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(pkt, ServerPacket::UnlinkAck));
        Ok(())
    }

    #[test]
    fn poll_echo_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_poll(&mut buf)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(pkt, ServerPacket::PollEcho));
        Ok(())
    }

    #[test]
    fn link2_reply_accept_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Accept)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(
            pkt,
            ServerPacket::Link2Reply {
                result: Link2Result::Accept
            }
        ));
        Ok(())
    }

    #[test]
    fn link2_reply_busy_roundtrip() -> TestResult {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, Link2Result::Busy)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        assert!(matches!(
            pkt,
            ServerPacket::Link2Reply {
                result: Link2Result::Busy
            }
        ));
        Ok(())
    }

    #[test]
    fn link2_reply_unknown_records_diagnostic() -> TestResult {
        // 8-byte reply with a tag that's neither OKRW nor BUSY — should still
        // parse as Link2Reply { Unknown } and fire Diagnostic::UnknownLink2Reply.
        let bytes = [0x08, 0xC0, 0x04, 0x00, b'F', b'A', b'I', b'L'];
        let mut sink = crate::validator::VecSink::default();
        let pkt = decode_server_to_client(&bytes, &mut sink)?;
        assert!(matches!(
            pkt,
            ServerPacket::Link2Reply {
                result: Link2Result::Unknown { .. }
            }
        ));
        assert_eq!(sink.len(), 1);
        Ok(())
    }

    // ─── Voice frames ─────────────────────────────────────────
    #[test]
    fn voice_header_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_header(&mut buf, sid(0xCAFE), &test_header())?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
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
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let mut buf = [0u8; 64];
        let n = encode_voice_data(&mut buf, sid(0x1234), 5, &frame)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
        match pkt {
            ServerPacket::VoiceData {
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
    fn voice_eot_server_roundtrip() -> TestResult {
        let mut buf = [0u8; 64];
        let n = encode_voice_eot(&mut buf, sid(0x1234), 7)?;
        let mut sink = NullSink;
        let pkt = decode_server_to_client(buf.get(..n).ok_or("n within buf")?, &mut sink)?;
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

    // ─── Error cases ──────────────────────────────────────────
    #[test]
    fn unknown_length_returns_error() -> TestResult {
        let mut sink = NullSink;
        let Err(err) = decode_client_to_server(&[0u8; 11], &mut sink) else {
            return Err("expected error for bad length".into());
        };
        assert!(matches!(err, DPlusError::UnknownPacketLength { got: 11 }));
        Ok(())
    }

    #[test]
    fn short_5_byte_with_bad_control_byte() -> TestResult {
        let mut sink = NullSink;
        let Err(err) = decode_client_to_server(&[0x05, 0x00, 0x18, 0x00, 0x77], &mut sink) else {
            return Err("expected error for bad control byte".into());
        };
        assert!(matches!(
            err,
            DPlusError::InvalidShortControlByte { byte: 0x77 }
        ));
        Ok(())
    }

    #[test]
    fn client_rejects_8_byte_server_reply() -> TestResult {
        // 8-byte LINK2 reply is server-only.
        let bytes = [0x08, 0xC0, 0x04, 0x00, b'O', b'K', b'R', b'W'];
        let mut sink = NullSink;
        let Err(err) = decode_client_to_server(&bytes, &mut sink) else {
            return Err("expected error for client rejecting 8-byte server reply".into());
        };
        assert!(matches!(err, DPlusError::UnknownPacketLength { got: 8 }));
        Ok(())
    }
}
