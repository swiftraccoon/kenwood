//! `DPlus` packet enums.
//!
//! `ClientPacket` represents every packet a `DPlus` client sends to a
//! reflector. `ServerPacket` represents every packet a reflector
//! sends to a client. The codec is symmetric — both directions are
//! first-class.

use crate::header::DStarHeader;
use crate::types::{Callsign, StreamId};
use crate::voice::VoiceFrame;

/// Packets the **client** sends (and the server receives).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientPacket {
    /// 5-byte LINK1 connect request `[0x05, 0x00, 0x18, 0x00, 0x01]`.
    Link1,

    /// 28-byte LINK2 login with callsign at `[4..]` and `b"DV019999"` at `[20..28]`.
    Link2 {
        /// Logging-in client callsign.
        callsign: Callsign,
    },

    /// 5-byte unlink `[0x05, 0x00, 0x18, 0x00, 0x00]`.
    Unlink,

    /// 3-byte keepalive poll `[0x03, 0x60, 0x00]`.
    Poll,

    /// 58-byte voice header (DSVT framed).
    VoiceHeader {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header (lenient — bytes preserved verbatim).
        header: DStarHeader,
    },

    /// 29-byte voice data (DSVT framed).
    VoiceData {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number (0..21 cycle).
        seq: u8,
        /// 9 AMBE bytes + 3 slow data bytes.
        frame: VoiceFrame,
    },

    /// 32-byte voice EOT (DSVT framed, AMBE silence + `END_PATTERN`).
    VoiceEot {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Final seq value (0x40 bit will be OR'd in by the encoder).
        seq: u8,
    },
}

/// Packets the **server** sends (and the client receives).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerPacket {
    /// 5-byte LINK1 ACK echo (server replies to client's LINK1).
    Link1Ack,

    /// 8-byte LINK2 reply: OKRW or BUSY/banned/unknown.
    Link2Reply {
        /// Tag at offsets `[4..8]`.
        result: Link2Result,
    },

    /// 28-byte LINK2 echo form some servers send instead of OKRW.
    Link2Echo {
        /// Echoed callsign.
        callsign: Callsign,
    },

    /// 5-byte UNLINK ACK echo.
    UnlinkAck,

    /// 3-byte poll echo.
    PollEcho,

    /// 58-byte voice header forwarded to a connected client.
    VoiceHeader {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: DStarHeader,
    },

    /// 29-byte voice data.
    VoiceData {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// 9 AMBE bytes + 3 slow data bytes.
        frame: VoiceFrame,
    },

    /// 32-byte voice EOT.
    VoiceEot {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Final seq value.
        seq: u8,
    },
}

/// Result of a `DPlus` LINK2 reply.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link2Result {
    /// Server returned `b"OKRW"` — login accepted.
    Accept,
    /// Server returned `b"BUSY"` — login refused.
    Busy,
    /// Server returned a 4-byte tag that doesn't match any known reply.
    Unknown {
        /// The 4-byte tag, typically interpreted as ASCII.
        reply: [u8; 4],
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_packet_link1_constructible() {
        let p = ClientPacket::Link1;
        assert!(matches!(p, ClientPacket::Link1));
    }

    #[test]
    fn client_packet_link2_carries_callsign() {
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        let p = ClientPacket::Link2 { callsign: cs };
        assert!(
            matches!(&p, ClientPacket::Link2 { callsign } if callsign.as_str() == "W1AW"),
            "expected Link2 with W1AW, got {p:?}"
        );
    }

    #[test]
    fn server_packet_link2_reply_accept() {
        let p = ServerPacket::Link2Reply {
            result: Link2Result::Accept,
        };
        assert!(
            matches!(p, ServerPacket::Link2Reply { result } if result == Link2Result::Accept),
            "expected Link2Reply/Accept, got {p:?}"
        );
    }

    #[test]
    fn link2_result_unknown_carries_reply_bytes() {
        let r = Link2Result::Unknown { reply: *b"FAIL" };
        assert!(
            matches!(r, Link2Result::Unknown { reply } if reply == *b"FAIL"),
            "expected Unknown with FAIL, got {r:?}"
        );
    }
}
