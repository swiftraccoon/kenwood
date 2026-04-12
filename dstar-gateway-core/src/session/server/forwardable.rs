//! `ForwardableFrame` — encoded voice frame ready for fan-out.
//!
//! The fan-out engine in `dstar-gateway-server` uses this type to
//! describe a voice frame in "ready to re-send" form. The idea is
//! that the frame is already encoded in its wire format, so the
//! fan-out loop can write the same bytes to N clients without
//! re-encoding.

use crate::types::{ProtocolKind, StreamId};

/// A voice frame ready to be forwarded to other connected clients.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum ForwardableFrame<'a> {
    /// Voice header (first packet of a stream).
    Header {
        /// Protocol of the originating client.
        protocol: ProtocolKind,
        /// Stream id.
        stream_id: StreamId,
        /// Encoded wire bytes (borrowed from the receive buffer).
        bytes: &'a [u8],
    },
    /// Voice data frame (middle of a stream).
    Data {
        /// Protocol of the originating client.
        protocol: ProtocolKind,
        /// Stream id.
        stream_id: StreamId,
        /// Frame seq.
        seq: u8,
        /// Encoded wire bytes.
        bytes: &'a [u8],
    },
    /// Voice EOT (final packet of a stream).
    Eot {
        /// Protocol of the originating client.
        protocol: ProtocolKind,
        /// Stream id.
        stream_id: StreamId,
        /// Final seq.
        seq: u8,
        /// Encoded wire bytes.
        bytes: &'a [u8],
    },
}

impl<'a> ForwardableFrame<'a> {
    /// The protocol this frame belongs to.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolKind {
        match self {
            Self::Header { protocol, .. }
            | Self::Data { protocol, .. }
            | Self::Eot { protocol, .. } => *protocol,
        }
    }

    /// The stream id this frame belongs to.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        match self {
            Self::Header { stream_id, .. }
            | Self::Data { stream_id, .. }
            | Self::Eot { stream_id, .. } => *stream_id,
        }
    }

    /// The wire bytes ready for forwarding.
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        match self {
            Self::Header { bytes, .. } | Self::Data { bytes, .. } | Self::Eot { bytes, .. } => {
                bytes
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ForwardableFrame, ProtocolKind, StreamId};

    #[expect(clippy::unwrap_used, reason = "const-validated: n is non-zero")]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
    }

    #[test]
    fn header_accessors_work() {
        let sid = sid(0x1234);
        let frame = ForwardableFrame::Header {
            protocol: ProtocolKind::DExtra,
            stream_id: sid,
            bytes: &[1, 2, 3, 4, 5],
        };
        assert_eq!(frame.protocol(), ProtocolKind::DExtra);
        assert_eq!(frame.stream_id(), sid);
        assert_eq!(frame.bytes(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn data_accessors_work() {
        let sid = sid(0xABCD);
        let frame = ForwardableFrame::Data {
            protocol: ProtocolKind::DPlus,
            stream_id: sid,
            seq: 5,
            bytes: &[0xAA; 27],
        };
        assert_eq!(frame.protocol(), ProtocolKind::DPlus);
        assert_eq!(frame.stream_id(), sid);
        assert_eq!(frame.bytes().len(), 27);
    }

    #[test]
    fn eot_accessors_work() {
        let sid = sid(0x0101);
        let frame = ForwardableFrame::Eot {
            protocol: ProtocolKind::Dcs,
            stream_id: sid,
            seq: 0x40,
            bytes: &[0xFF; 100],
        };
        assert_eq!(frame.protocol(), ProtocolKind::Dcs);
        assert_eq!(frame.stream_id(), sid);
        assert_eq!(frame.bytes().len(), 100);
    }
}
