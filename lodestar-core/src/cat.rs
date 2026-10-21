// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Minimal CAT command codec exposed to Swift.
//!
//! Covers the `ID` identify and `FV` firmware-version commands used to
//! qualify a radio before firmware-specific MCP access. The shared
//! [codec workflow](crate#pure-codecs-and-audio) covers these operations.

/// CAT commands that Lodestar can send over the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatCommand {
    /// Radio identity query: `ID\r`.
    Identify,
    /// Firmware-version query: `FV\r`.
    FirmwareVersion,
}

impl CatCommand {
    /// Encode this query with its final carriage return.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::Identify => b"ID\r".to_vec(),
            Self::FirmwareVersion => b"FV\r".to_vec(),
        }
    }
}

impl From<CatCommand> for Vec<u8> {
    fn from(value: CatCommand) -> Self {
        value.encode()
    }
}

/// Parsed CAT response received from the radio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatResponse {
    /// Identity response: model string after the `ID ` prefix.
    Identify {
        /// Radio model reported by the `ID` response, e.g. `TH-D75A`.
        model: String,
    },
    /// Firmware-version response: version string after the `FV ` prefix.
    FirmwareVersion {
        /// Radio firmware version, e.g. `1.03`.
        version: String,
    },
    /// Radio returned `?`, meaning unknown command.
    Unknown,
    /// Radio returned `N`, meaning command not available in current mode.
    NotAvailableInMode,
    /// Raw wire bytes that didn't match any known shape.
    Raw {
        /// The trimmed UTF-8 (lossy) representation of the line.
        line: String,
    },
}

/// Encode a [`CatCommand`] to the wire bytes the radio expects.
#[must_use]
pub fn encode_cat(command: CatCommand) -> Vec<u8> {
    command.encode()
}

impl CatResponse {
    /// Parse a single CAT line, with or without its carriage return.
    ///
    /// Invalid UTF-8 is replaced lossily; leading/trailing Unicode whitespace
    /// is trimmed. Unknown shapes remain [`Self::Raw`] diagnostic observations.
    #[must_use]
    pub fn parse(line: &[u8]) -> Self {
        let text = String::from_utf8_lossy(line).trim().to_owned();

        if text == "?" {
            return Self::Unknown;
        }
        if text == "N" {
            return Self::NotAvailableInMode;
        }
        if let Some(rest) = text.strip_prefix("ID ") {
            return Self::Identify {
                model: rest.trim().to_owned(),
            };
        }
        if let Some(rest) = text.strip_prefix("FV ") {
            return Self::FirmwareVersion {
                version: rest.trim().to_owned(),
            };
        }
        Self::Raw { line: text }
    }
}

impl From<&[u8]> for CatResponse {
    fn from(value: &[u8]) -> Self {
        Self::parse(value)
    }
}

/// Foreign UDL entry point for [`CatResponse::parse`].
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UDL byte sequences transfer owned vectors across the FFI"
)]
pub fn parse_cat_line(line: Vec<u8>) -> CatResponse {
    CatResponse::parse(&line)
}

#[cfg(test)]
mod tests {
    use super::{CatCommand, CatResponse, encode_cat, parse_cat_line};

    #[test]
    fn encode_identify_is_id_cr() {
        assert_eq!(encode_cat(CatCommand::Identify), b"ID\r");
    }

    #[test]
    fn encode_firmware_version_is_fv_cr() {
        assert_eq!(encode_cat(CatCommand::FirmwareVersion), b"FV\r");
    }

    #[test]
    fn parse_identity_response() {
        let r = parse_cat_line(b"ID TH-D75A".to_vec());
        assert_eq!(
            r,
            CatResponse::Identify {
                model: "TH-D75A".to_owned(),
            },
        );
    }

    #[test]
    fn parse_firmware_version_response() {
        let r = parse_cat_line(b"FV 1.03".to_vec());
        assert_eq!(
            r,
            CatResponse::FirmwareVersion {
                version: "1.03".to_owned(),
            },
        );
    }

    #[test]
    fn parse_unknown_response() {
        assert_eq!(parse_cat_line(b"?".to_vec()), CatResponse::Unknown);
    }

    #[test]
    fn parse_not_available_response() {
        assert_eq!(
            parse_cat_line(b"N".to_vec()),
            CatResponse::NotAvailableInMode
        );
    }

    #[test]
    fn parse_unknown_shape_is_raw() {
        assert_eq!(
            parse_cat_line(b"WEIRD STUFF".to_vec()),
            CatResponse::Raw {
                line: "WEIRD STUFF".to_owned(),
            },
        );
    }

    #[test]
    fn parse_strips_leading_trailing_whitespace() {
        let r = parse_cat_line(b"  ID TH-D75E  ".to_vec());
        assert_eq!(
            r,
            CatResponse::Identify {
                model: "TH-D75E".to_owned(),
            },
        );
    }
}
