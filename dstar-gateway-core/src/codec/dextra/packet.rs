//! `DExtra` packet enums.
//!
//! `ClientPacket` represents every packet a `DExtra` client sends to a
//! reflector. `ServerPacket` represents every packet a reflector sends
//! to a client. The codec is symmetric — both directions are
//! first-class.

use crate::header::DStarHeader;
use crate::types::{Callsign, Module, StreamId};
use crate::voice::VoiceFrame;

/// Packets a `DExtra` client sends to a reflector.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientPacket {
    /// 11-byte LINK request.
    Link {
        /// Logging-in client callsign.
        callsign: Callsign,
        /// Module on the reflector to link to.
        reflector_module: Module,
        /// Client's local module letter.
        client_module: Module,
    },

    /// 11-byte UNLINK request: same shape but reflector module is space.
    Unlink {
        /// Logging-out client callsign.
        callsign: Callsign,
        /// Client's local module letter.
        client_module: Module,
    },

    /// 9-byte keepalive poll.
    Poll {
        /// Polling client callsign.
        callsign: Callsign,
    },

    /// 56-byte voice header (DSVT framed, no `DPlus` prefix).
    VoiceHeader {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: DStarHeader,
    },

    /// 27-byte voice data (DSVT framed).
    VoiceData {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// 9 AMBE bytes + 3 slow data bytes.
        frame: VoiceFrame,
    },

    /// 27-byte voice EOT (DSVT framed, seq has 0x40 bit).
    VoiceEot {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Final seq value (encoder OR's in 0x40).
        seq: u8,
    },
}

/// Packets a `DExtra` reflector sends to a client.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerPacket {
    /// 14-byte connect ACK: echoed callsign + module at `[0..10]`,
    /// `b"ACK"` at `[10..13]`, NUL at `[13]`.
    ConnectAck {
        /// Echoed callsign.
        callsign: Callsign,
        /// Echoed reflector module.
        reflector_module: Module,
    },

    /// 14-byte connect NAK: echoed callsign + module at `[0..10]`,
    /// `b"NAK"` at `[10..13]`, NUL at `[13]`.
    ConnectNak {
        /// Echoed callsign.
        callsign: Callsign,
        /// Echoed reflector module.
        reflector_module: Module,
    },

    /// 9-byte poll echo.
    PollEcho {
        /// Echoed callsign.
        callsign: Callsign,
    },

    /// 56-byte voice header.
    VoiceHeader {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Decoded header.
        header: DStarHeader,
    },

    /// 27-byte voice data.
    VoiceData {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// Voice frame.
        frame: VoiceFrame,
    },

    /// 27-byte voice EOT.
    VoiceEot {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Final seq.
        seq: u8,
    },
}

/// Result of a `DExtra` connect attempt.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectResult {
    /// `b"ACK"` reply.
    Accept,
    /// `b"NAK"` reply.
    Reject,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_packet_link_constructible() {
        let p = ClientPacket::Link {
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            reflector_module: Module::C,
            client_module: Module::B,
        };
        assert!(matches!(p, ClientPacket::Link { .. }));
    }

    #[test]
    fn server_packet_connect_ack_carries_module() {
        let p = ServerPacket::ConnectAck {
            callsign: Callsign::from_wire_bytes(*b"XRF030  "),
            reflector_module: Module::C,
        };
        assert!(
            matches!(p, ServerPacket::ConnectAck { reflector_module, .. } if reflector_module == Module::C),
            "expected ConnectAck with Module::C, got {p:?}"
        );
    }

    #[test]
    fn connect_result_variants_distinct() {
        assert_ne!(ConnectResult::Accept, ConnectResult::Reject);
    }
}
