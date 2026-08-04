//! AX.25 PID (Protocol Identifier) field classification.

use crate::error::Ax25Error;

/// An unassigned one-octet AX.25 protocol identifier.
///
/// Values with named [`Ax25Pid`] variants and the `0xFF` escape prefix cannot
/// be represented here. Obtain this value by matching the [`Ax25Pid::Other`]
/// result of [`Ax25Pid::from_byte`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnknownPid(u8);

impl UnknownPid {
    /// Return the unassigned PID octet.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }

    const fn from_parsed(value: u8) -> Self {
        Self(value)
    }
}

/// AX.25 PID (Protocol Identifier) field.
///
/// Per AX.25 v2.2 §3.4 Figure 3.2. Only a small subset is observed on APRS
/// (`0xF0` = no layer 3). Most identifiers occupy one octet; `0xFF` is an
/// escape prefix and requires a second protocol octet, represented by
/// [`Self::Escape`].
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
    /// Escaped two-octet protocol identifier.
    Escape {
        /// Required octet following the `0xFF` escape prefix.
        extension: u8,
    },
    /// Any unassigned one-octet identifier the library does not classify.
    Other(UnknownPid),
}

impl Ax25Pid {
    /// Parse a complete one-octet PID field.
    ///
    /// The `0xFF` escape prefix is not a complete PID by itself; construct it
    /// with [`Self::escaped`] once its required extension octet is available.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::MissingProtocolIdentifierExtension`] for `0xFF`.
    pub const fn from_byte(b: u8) -> Result<Self, Ax25Error> {
        match b {
            0x01 => Ok(Self::Iso8208),
            0x06 => Ok(Self::CompressedTcpIp),
            0x07 => Ok(Self::UncompressedTcpIp),
            0x08 => Ok(Self::SegmentationFragment),
            0xC3 => Ok(Self::TexNet),
            0xC4 => Ok(Self::LinkQuality),
            0xCA => Ok(Self::Appletalk),
            0xCB => Ok(Self::AppletalkArp),
            0xCC => Ok(Self::Ip),
            0xCD => Ok(Self::Arp),
            0xCE => Ok(Self::FlexNet),
            0xCF => Ok(Self::NetRom),
            0xF0 => Ok(Self::NoLayer3),
            0xFF => Err(Ax25Error::MissingProtocolIdentifierExtension),
            other => Ok(Self::Other(UnknownPid::from_parsed(other))),
        }
    }

    /// Construct a complete escaped PID from its required extension octet.
    #[must_use]
    pub const fn escaped(extension: u8) -> Self {
        Self::Escape { extension }
    }

    /// Return the first PID octet.
    ///
    /// For [`Self::Escape`], this returns the `0xFF` prefix; use
    /// [`Self::extension_byte`] for the required second octet.
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
            Self::Escape { .. } => 0xFF,
            Self::Other(pid) => pid.as_byte(),
        }
    }

    /// Return the required second PID octet for an escaped identifier.
    #[must_use]
    pub const fn extension_byte(self) -> Option<u8> {
        match self {
            Self::Escape { extension } => Some(extension),
            _ => None,
        }
    }

    /// Return the encoded PID field length in octets.
    #[must_use]
    pub const fn wire_len(self) -> usize {
        if self.extension_byte().is_some() {
            2
        } else {
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::Ax25Pid;
    use crate::error::Ax25Error;

    type TestResult = Result<(), Box<dyn core::error::Error>>;

    #[test]
    fn every_complete_single_octet_pid_roundtrips() -> TestResult {
        for b in 0..=255u8 {
            if b == 0xFF {
                continue;
            }
            let pid = Ax25Pid::from_byte(b)?;
            assert_eq!(pid.as_byte(), b, "PID {b:#04x} must survive decode→encode");
            assert_eq!(pid.extension_byte(), None);
            assert_eq!(pid.wire_len(), 1);
        }
        Ok(())
    }

    #[test]
    fn bare_escape_prefix_is_not_a_complete_pid() {
        assert_eq!(
            Ax25Pid::from_byte(0xFF),
            Err(Ax25Error::MissingProtocolIdentifierExtension),
        );
    }

    #[test]
    fn every_escape_extension_is_preserved() {
        for extension in 0..=u8::MAX {
            let pid = Ax25Pid::escaped(extension);
            assert_eq!(pid.as_byte(), 0xFF);
            assert_eq!(pid.extension_byte(), Some(extension));
            assert_eq!(pid.wire_len(), 2);
        }
    }
}
