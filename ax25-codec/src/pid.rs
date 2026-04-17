//! AX.25 PID (Protocol Identifier) byte classification.

/// AX.25 PID (Protocol Identifier) byte.
///
/// Per AX.25 v2.2 §2.2.4 Table 2. Only a small subset is observed on APRS
/// (`0xF0` = no layer 3) but the full enum lets the library parse and
/// build any AX.25 frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ax25Pid {
    /// ISO 8208 / X.25 PLP (layer 3).
    Iso8208,
    /// Compressed TCP/IP packet (Van Jacobson, RFC 1144).
    CompressedTcpIp,
    /// Uncompressed TCP/IP packet (Van Jacobson, RFC 1144).
    UncompressedTcpIp,
    /// Segmentation fragment (AX.25 §4.3.2.10).
    SegmentationFragment,
    /// TEXNET datagram protocol.
    TexNet,
    /// Link Quality Protocol.
    LinkQuality,
    /// Appletalk.
    Appletalk,
    /// Appletalk ARP.
    AppletalkArp,
    /// Internet protocol (RFC 791).
    Ip,
    /// Address Resolution Protocol.
    Arp,
    /// `FlexNet`.
    FlexNet,
    /// `NET/ROM` protocol.
    NetRom,
    /// No layer-3 protocol (the APRS case, `0xF0`).
    NoLayer3,
    /// Escape character: next byte defines the protocol.
    Escape,
    /// Any other raw byte the library does not classify.
    Other(u8),
}

impl Ax25Pid {
    /// Parse a single PID byte into an enum value.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Self::Iso8208,
            0x06 => Self::CompressedTcpIp,
            0x07 => Self::UncompressedTcpIp,
            0x08 => Self::SegmentationFragment,
            0xC3 => Self::TexNet,
            0xC4 => Self::LinkQuality,
            0xCA => Self::Appletalk,
            0xCB => Self::AppletalkArp,
            0xCC => Self::Ip,
            0xCD => Self::Arp,
            0xCE => Self::FlexNet,
            0xCF => Self::NetRom,
            0xF0 => Self::NoLayer3,
            0xFF => Self::Escape,
            other => Self::Other(other),
        }
    }

    /// Convert back to the raw PID byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Iso8208 => 0x01,
            Self::CompressedTcpIp => 0x06,
            Self::UncompressedTcpIp => 0x07,
            Self::SegmentationFragment => 0x08,
            Self::TexNet => 0xC3,
            Self::LinkQuality => 0xC4,
            Self::Appletalk => 0xCA,
            Self::AppletalkArp => 0xCB,
            Self::Ip => 0xCC,
            Self::Arp => 0xCD,
            Self::FlexNet => 0xCE,
            Self::NetRom => 0xCF,
            Self::NoLayer3 => 0xF0,
            Self::Escape => 0xFF,
            Self::Other(b) => b,
        }
    }
}
