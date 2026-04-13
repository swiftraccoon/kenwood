// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Parsed `GetVersion` response.
//!
//! The response payload starts with a protocol-version byte, then
//! (for protocol v2) the CAP1/CAP2 bytes and a CPU/UDID block, then
//! a human-readable description string.
//!
//! Mirrors the parsing in `ref/MMDVMHost/Modem.cpp:1996-2049`.

use crate::capabilities::Capabilities;
use crate::error::MmdvmError;

/// Index (within the payload) where the description starts for
/// protocol v1 responses. The payload layout is:
/// `[protocol][description bytes...]` — protocol at 0, description
/// at 1.
const V1_DESCRIPTION_OFFSET: usize = 1;

/// Index where the description starts for protocol v2 responses.
/// Layout: `[protocol, cap1, cap2, cpu_type, udid[0..16], description...]`.
/// That's 1 + 2 + 1 + 16 = 20 bytes before the description starts.
const V2_DESCRIPTION_OFFSET: usize = 20;

/// Parsed `GetVersion` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionResponse {
    /// Protocol version (1 or 2).
    pub protocol: u8,
    /// Firmware description string (e.g. `"MMDVM 20200101"`,
    /// `"MMDVM_HS-Hat v1.5.4"`).
    pub description: String,
    /// Capability bitfields (present only on protocol v2).
    pub capabilities: Option<Capabilities>,
}

impl VersionResponse {
    /// Parse a `GetVersion` response payload.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::InvalidVersionResponse`] if the payload
    /// is empty (no protocol byte).
    pub fn parse(payload: &[u8]) -> Result<Self, MmdvmError> {
        let Some(&protocol) = payload.first() else {
            return Err(MmdvmError::InvalidVersionResponse);
        };

        if protocol == 2 {
            // Need protocol(0) + cap1(1) + cap2(2) at minimum.
            let cap1 = payload
                .get(1)
                .copied()
                .ok_or(MmdvmError::InvalidVersionResponse)?;
            let cap2 = payload
                .get(2)
                .copied()
                .ok_or(MmdvmError::InvalidVersionResponse)?;
            let description = decode_description(payload, V2_DESCRIPTION_OFFSET);
            return Ok(Self {
                protocol,
                description,
                capabilities: Some(Capabilities::new(cap1, cap2)),
            });
        }
        // Protocol v1 (or older / unknown) — description starts
        // right after the protocol byte; no capability bits
        // are present on the wire.
        let description = decode_description(payload, V1_DESCRIPTION_OFFSET);
        Ok(Self {
            protocol,
            description,
            capabilities: None,
        })
    }
}

/// Decode the trailing description string, stripping any NUL
/// terminator and trailing whitespace.
fn decode_description(payload: &[u8], offset: usize) -> String {
    let tail = payload.get(offset..).unwrap_or(&[]);
    let s = String::from_utf8_lossy(tail);
    s.trim_end_matches('\0').trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_v1_basic() -> TestResult {
        let payload = [1u8, b'M', b'M', b'D', b'V', b'M', b' ', b'2', b'0'];
        let v = VersionResponse::parse(&payload)?;
        assert_eq!(v.protocol, 1);
        assert_eq!(v.description, "MMDVM 20");
        assert!(v.capabilities.is_none());
        Ok(())
    }

    #[test]
    fn parse_v1_strips_nul_and_trailing_ws() -> TestResult {
        let payload = [
            1u8, b'M', b'M', b'D', b'V', b'M', b' ', b'2', b'0', b' ', 0, 0,
        ];
        let v = VersionResponse::parse(&payload)?;
        assert_eq!(v.description, "MMDVM 20");
        Ok(())
    }

    #[test]
    fn parse_v2_with_capabilities() -> TestResult {
        // proto=2, cap1=CAP1_DSTAR|CAP1_FM, cap2=CAP2_POCSAG, cpu=0,
        // udid[0..16] = zeros, description "MMDVM_HS-v2".
        let mut payload = vec![2u8, 0x41, 0x01, 0];
        payload.extend_from_slice(&[0u8; 16]);
        payload.extend_from_slice(b"MMDVM_HS-v2");
        let v = VersionResponse::parse(&payload)?;
        assert_eq!(v.protocol, 2);
        assert_eq!(v.description, "MMDVM_HS-v2");
        let caps = v.capabilities.ok_or("expected capabilities on v2")?;
        assert!(caps.has_dstar());
        assert!(caps.has_fm());
        assert!(caps.has_pocsag());
        Ok(())
    }

    #[test]
    fn parse_empty_errors() {
        let err = VersionResponse::parse(&[]);
        assert!(
            matches!(err, Err(MmdvmError::InvalidVersionResponse)),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_v2_truncated_caps_errors() {
        let err = VersionResponse::parse(&[2, 0x01]);
        assert!(
            matches!(err, Err(MmdvmError::InvalidVersionResponse)),
            "got {err:?}"
        );
    }
}
