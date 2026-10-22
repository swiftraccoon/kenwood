// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Supported version responses with checked text and complete fixed headers.
//!
//! `MMDVMHost/Modem.cpp` reads version-1 text at payload offset 1 and
//! version-2 text at offset 20. The latter follows two capability bytes,
//! a CPU-type byte and a sixteen-byte device identifier.
//! See the [version example](crate#parse-version-and-status-responses).

use std::fmt;

use crate::capabilities::Capabilities;
use crate::error::MmdvmError;

/// A protocol version supported by the response parsers.
///
/// Conversion from a wire byte rejects unsupported versions. See the
/// [version example](crate#parse-version-and-status-responses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolVersion {
    /// Version 1: description follows the protocol byte.
    V1,
    /// Version 2: description follows the twenty-byte fixed header.
    V2,
}

impl ProtocolVersion {
    /// Return the protocol's wire byte, either 1 or 2.
    #[must_use]
    pub const fn get(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

impl TryFrom<u8> for ProtocolVersion {
    type Error = MmdvmError;

    fn try_from(protocol: u8) -> Result<Self, Self::Error> {
        match protocol {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            _ => Err(MmdvmError::UnsupportedProtocolVersion { protocol }),
        }
    }
}

impl From<ProtocolVersion> for u8 {
    fn from(protocol: ProtocolVersion) -> Self {
        protocol.get()
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.get(), formatter)
    }
}

/// A supported version response with validated, owned firmware text.
///
/// Protocol-1 responses carry no capability bytes; protocol-2 responses carry
/// exactly two, including any unrecognized bits. An empty description is
/// representable; a caller using it to identify a modem supplies its own
/// identity requirements. See the
/// [version example](crate#parse-version-and-status-responses).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VersionResponse {
    protocol: ProtocolVersion,
    description: String,
    capabilities: Option<Capabilities>,
}

impl VersionResponse {
    /// Parse a complete version-response payload.
    ///
    /// Version 1 requires its protocol byte; version 2 requires all twenty
    /// fixed-header bytes. Description bytes must be UTF-8. Trailing NULs and
    /// ASCII whitespace are removed together; controls within the remaining
    /// description are rejected. The caller retains the original wire bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::InvalidVersionResponse`] for an incomplete header,
    /// [`MmdvmError::UnsupportedProtocolVersion`] for another protocol byte,
    /// [`MmdvmError::InvalidVersionText`] for invalid UTF-8, or
    /// [`MmdvmError::VersionDescriptionControl`] for embedded control characters.
    pub fn parse(payload: &[u8]) -> Result<Self, MmdvmError> {
        let protocol = ProtocolVersion::try_from(
            payload
                .first()
                .copied()
                .ok_or(MmdvmError::InvalidVersionResponse)?,
        )?;
        let (offset, capabilities) = match protocol {
            ProtocolVersion::V1 => (1, None),
            ProtocolVersion::V2 => {
                let cap1 = payload
                    .get(1)
                    .copied()
                    .ok_or(MmdvmError::InvalidVersionResponse)?;
                let cap2 = payload
                    .get(2)
                    .copied()
                    .ok_or(MmdvmError::InvalidVersionResponse)?;
                (20, Some(Capabilities::from_bytes([cap1, cap2])))
            }
        };
        let raw = payload
            .get(offset..)
            .ok_or(MmdvmError::InvalidVersionResponse)?;
        let text = std::str::from_utf8(raw).map_err(|error| MmdvmError::InvalidVersionText {
            valid_up_to: error.valid_up_to(),
            error_len: error.error_len(),
        })?;
        let description = text.trim_end_matches(|character: char| {
            character == '\0' || character.is_ascii_whitespace()
        });
        if let Some((byte_offset, character)) = description
            .char_indices()
            .find(|(_, character)| character.is_control())
        {
            return Err(MmdvmError::VersionDescriptionControl {
                byte_offset,
                character,
            });
        }
        Ok(Self {
            protocol,
            description: description.to_owned(),
            capabilities,
        })
    }

    /// Return the supported protocol that selects the status layout.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Borrow the validated description after trailing-padding removal.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Return the two wire capability bytes on protocol 2, or none on protocol 1.
    #[must_use]
    pub const fn capabilities(&self) -> Option<Capabilities> {
        self.capabilities
    }

    /// Consume this response and transfer its description allocation.
    #[must_use]
    pub fn into_description(self) -> String {
        self.description
    }
}

impl TryFrom<&[u8]> for VersionResponse {
    type Error = MmdvmError;

    fn try_from(payload: &[u8]) -> Result<Self, Self::Error> {
        Self::parse(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn v2_requires_every_fixed_header_byte_before_accepting_a_reply() -> TestResult {
        let mut payload = vec![2, 0x41, 1, 0];
        payload.extend_from_slice(&[0; 16]);
        payload.extend_from_slice(b"MMDVM test");
        for length in 0..20 {
            let result = VersionResponse::parse(payload.get(..length).ok_or("prefix")?);
            assert!(
                matches!(result, Err(MmdvmError::InvalidVersionResponse)),
                "length={length}: {result:?}"
            );
        }
        let parsed = VersionResponse::parse(&payload)?;
        assert_eq!(parsed.protocol(), ProtocolVersion::V2);
        assert_eq!(
            parsed
                .capabilities()
                .ok_or("missing capabilities")?
                .to_bytes(),
            [0x41, 1]
        );
        assert_eq!(parsed.description(), "MMDVM test");
        assert_eq!(
            VersionResponse::parse(payload.get(..20).ok_or("header")?)?.description(),
            ""
        );
        Ok(())
    }

    #[test]
    fn unknown_versions_and_invalid_description_bytes_are_rejected() -> TestResult {
        for protocol in [0, 3, u8::MAX] {
            let result = VersionResponse::parse(&[protocol, b'A']);
            assert!(
                matches!(result, Err(MmdvmError::UnsupportedProtocolVersion { protocol: actual }) if actual == protocol),
                "{result:?}"
            );
        }
        let invalid = VersionResponse::parse(&[1, b'A', 0xFF]);
        assert!(
            matches!(
                invalid,
                Err(MmdvmError::InvalidVersionText {
                    valid_up_to: 1,
                    error_len: Some(1)
                })
            ),
            "{invalid:?}"
        );
        let invalid = VersionResponse::parse(&[1, b'A', 0, b'B']);
        assert!(
            matches!(
                invalid,
                Err(MmdvmError::VersionDescriptionControl {
                    byte_offset: 1,
                    character: '\0'
                })
            ),
            "{invalid:?}"
        );
        let parsed = VersionResponse::parse(b"\x01MMDVM test\0 \t\0")?;
        assert_eq!(parsed.protocol(), ProtocolVersion::V1);
        assert_eq!(parsed.capabilities(), None);
        assert_eq!(parsed.description(), "MMDVM test");
        Ok(())
    }
}
