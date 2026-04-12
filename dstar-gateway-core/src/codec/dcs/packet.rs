//! `DCS` packet enums.
//!
//! `ClientPacket` represents every packet a `DCS` client sends to a
//! reflector. `ServerPacket` represents every packet a reflector sends
//! to a client. The codec is symmetric — both directions are
//! first-class.

use crate::header::DStarHeader;
use crate::types::{Callsign, Module, StreamId};
use crate::voice::VoiceFrame;

/// Packets a `DCS` client sends to a reflector.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientPacket {
    /// 519-byte LINK request with embedded HTML client identifier.
    Link {
        /// Logging-in client callsign.
        callsign: Callsign,
        /// Client's local module letter.
        client_module: Module,
        /// Module on the reflector to link to.
        reflector_module: Module,
        /// The reflector callsign (e.g. `DCS001`).
        reflector_callsign: Callsign,
        /// Client gateway type (encoded in the HTML payload).
        gateway_type: GatewayType,
    },

    /// 19-byte UNLINK packet.
    Unlink {
        /// Logging-out client callsign.
        callsign: Callsign,
        /// Client's local module letter.
        client_module: Module,
        /// The reflector callsign.
        reflector_callsign: Callsign,
    },

    /// 17-byte keepalive poll request.
    Poll {
        /// Polling client callsign.
        callsign: Callsign,
        /// The reflector callsign.
        reflector_callsign: Callsign,
    },

    /// 100-byte voice frame (header + AMBE + slow data all embedded).
    Voice {
        /// Decoded D-STAR header embedded at bytes `[4..43]`.
        header: DStarHeader,
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// Voice frame (9 bytes AMBE + 3 bytes slow data).
        frame: VoiceFrame,
        /// True if this is the last frame of the stream.
        is_end: bool,
    },
}

/// Packets a `DCS` reflector sends to a client.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerPacket {
    /// 14-byte ACK reply to a LINK request.
    ConnectAck {
        /// Echoed callsign.
        callsign: Callsign,
        /// Echoed reflector module.
        reflector_module: Module,
    },

    /// 14-byte NAK reply to a LINK request.
    ConnectNak {
        /// Echoed callsign.
        callsign: Callsign,
        /// Echoed reflector module.
        reflector_module: Module,
    },

    /// 17-byte keepalive poll echo.
    PollEcho {
        /// Echoed callsign.
        callsign: Callsign,
        /// Echoed reflector callsign.
        reflector_callsign: Callsign,
    },

    /// 100-byte voice frame forwarded to a connected client.
    Voice {
        /// Decoded D-STAR header embedded at bytes `[4..43]`.
        header: DStarHeader,
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// Voice frame (9 bytes AMBE + 3 bytes slow data).
        frame: VoiceFrame,
        /// True if this is the last frame of the stream.
        is_end: bool,
    },
}

/// Result of a `DCS` connect attempt.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectResult {
    /// `b"ACK"` reply.
    Accept,
    /// `b"NAK"` reply.
    Reject,
}

/// Client gateway type embedded in the `DCS` LINK HTML payload.
///
/// Reference: `ircDDBGateway/Common/ConnectData.cpp:345-357` enumerates
/// the four cases the reference implementation writes into the HTML
/// template.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayType {
    /// Standard D-STAR repeater (default).
    Repeater,
    /// Client-mode hotspot.
    Hotspot,
    /// USB DV dongle.
    Dongle,
    /// `STARnet` digital voice group.
    StarNet,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_type_variants_distinct() {
        assert_ne!(GatewayType::Repeater, GatewayType::Hotspot);
        assert_ne!(GatewayType::Dongle, GatewayType::StarNet);
        assert_ne!(GatewayType::Repeater, GatewayType::Dongle);
    }

    const CS_W1AW: Callsign = Callsign::from_wire_bytes(*b"W1AW    ");
    const CS_DCS001: Callsign = Callsign::from_wire_bytes(*b"DCS001  ");

    #[test]
    fn client_packet_link_constructible() {
        let p = ClientPacket::Link {
            callsign: CS_W1AW,
            client_module: Module::B,
            reflector_module: Module::C,
            reflector_callsign: CS_DCS001,
            gateway_type: GatewayType::Hotspot,
        };
        assert!(matches!(p, ClientPacket::Link { .. }));
    }

    #[test]
    fn client_packet_unlink_constructible() {
        let p = ClientPacket::Unlink {
            callsign: CS_W1AW,
            client_module: Module::B,
            reflector_callsign: CS_DCS001,
        };
        assert!(matches!(p, ClientPacket::Unlink { .. }));
    }

    #[test]
    fn client_packet_poll_constructible() {
        let p = ClientPacket::Poll {
            callsign: CS_W1AW,
            reflector_callsign: CS_DCS001,
        };
        assert!(matches!(p, ClientPacket::Poll { .. }));
    }

    #[test]
    fn server_packet_connect_ack_carries_module() {
        let p = ServerPacket::ConnectAck {
            callsign: CS_DCS001,
            reflector_module: Module::C,
        };
        assert!(
            matches!(p, ServerPacket::ConnectAck { reflector_module, .. } if reflector_module == Module::C),
            "expected ConnectAck with Module::C, got {p:?}"
        );
    }

    #[test]
    fn server_packet_poll_echo_constructible() {
        let p = ServerPacket::PollEcho {
            callsign: CS_DCS001,
            reflector_callsign: CS_DCS001,
        };
        assert!(matches!(p, ServerPacket::PollEcho { .. }));
    }

    #[test]
    fn connect_result_variants_distinct() {
        assert_ne!(ConnectResult::Accept, ConnectResult::Reject);
    }
}
