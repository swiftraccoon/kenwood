//! The CAT identity family: `ID`, `FV`, `TY`.
//!
//! Requests are the mnemonic plus a carriage return; replies are the
//! mnemonic, a space, and the payload, terminated the same way. A lone `?`
//! is a rejection and a lone `N` means the command is unavailable in the
//! current mode.

use crate::error::ProtocolError;
use crate::types::{FirmwareIdentity, MarketType, RadioModel};

/// Byte that terminates every CAT line.
pub const LINE_TERMINATOR: u8 = b'\r';

/// Identity commands this crate sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `ID`: model string.
    Identify,
    /// `FV`: firmware identity.
    FirmwareVersion,
    /// `TY`: market type byte.
    RadioType,
}

impl Command {
    /// The two-letter mnemonic.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Identify => "ID",
            Self::FirmwareVersion => "FV",
            Self::RadioType => "TY",
        }
    }

    /// The bytes to send.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut bytes = self.mnemonic().as_bytes().to_vec();
        bytes.push(LINE_TERMINATOR);
        bytes
    }
}

/// A parsed reply line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// `ID TM-D750`.
    Identity {
        /// The model.
        model: RadioModel,
    },
    /// `FV <identity>`.
    FirmwareVersion {
        /// The firmware identity.
        version: FirmwareIdentity,
    },
    /// `TY <byte>`.
    RadioType {
        /// The market type byte.
        market: MarketType,
    },
    /// `?`: the radio rejected the command.
    Rejected,
    /// `N`: the command is unavailable in the current mode.
    NotAvailable,
    /// Any other line, kept verbatim.
    Other {
        /// Mnemonic.
        mnemonic: String,
        /// Payload after the space (empty when absent).
        payload: String,
    },
}

/// Parse one reply line (with or without its terminator).
///
/// # Errors
///
/// Returns [`ProtocolError::NonAsciiLine`] for non-ASCII bytes,
/// [`ProtocolError::EmptyLine`] for an empty line,
/// [`ProtocolError::UnexpectedIdentity`] for an `ID` payload other than
/// `TM-D750`, and [`ProtocolError::FieldParse`] for a malformed `FV` or `TY` payload.
pub fn parse_line(line: &[u8]) -> Result<Response, ProtocolError> {
    let line = line.strip_suffix(&[LINE_TERMINATOR]).unwrap_or(line);
    if !line.is_ascii() {
        return Err(ProtocolError::NonAsciiLine {
            line: line.to_vec(),
        });
    }
    let text = String::from_utf8_lossy(line);
    let text = text.as_ref();
    match text {
        "?" => return Ok(Response::Rejected),
        "N" => return Ok(Response::NotAvailable),
        "" => {
            return Err(ProtocolError::EmptyLine {
                line: String::new(),
            });
        }
        _ => {}
    }
    let (mnemonic, payload) = text.split_once(' ').unwrap_or((text, ""));
    match mnemonic {
        "ID" => RadioModel::try_from(payload)
            .map(|model| Response::Identity { model })
            .map_err(|_| ProtocolError::UnexpectedIdentity {
                reply: payload.to_owned(),
            }),
        "FV" => FirmwareIdentity::new(payload)
            .map(|version| Response::FirmwareVersion { version })
            .map_err(|error| ProtocolError::FieldParse {
                command: "FV",
                field: "version",
                detail: error.to_string(),
            }),
        "TY" => payload
            .bytes()
            .next()
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "TY",
                field: "type",
                detail: "empty payload".to_owned(),
            })
            .and_then(|byte| {
                MarketType::new(byte).map_err(|error| ProtocolError::FieldParse {
                    command: "TY",
                    field: "type",
                    detail: error.to_string(),
                })
            })
            .map(|market| Response::RadioType { market }),
        other => Ok(Response::Other {
            mnemonic: other.to_owned(),
            payload: payload.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn commands_encode_with_a_carriage_return() {
        assert_eq!(Command::Identify.encode(), b"ID\r");
        assert_eq!(Command::FirmwareVersion.encode(), b"FV\r");
        assert_eq!(Command::RadioType.encode(), b"TY\r");
    }

    #[test]
    fn identity_replies_parse() -> TestResult {
        assert_eq!(
            parse_line(b"ID TM-D750\r")?,
            Response::Identity {
                model: RadioModel::TmD750
            }
        );
        assert_eq!(
            parse_line(b"FV 1.00")?,
            Response::FirmwareVersion {
                version: FirmwareIdentity::new("1.00")?
            }
        );
        assert_eq!(
            parse_line(b"TY J")?,
            Response::RadioType {
                market: MarketType::new(b'J')?
            }
        );
        assert_eq!(parse_line(b"?")?, Response::Rejected);
        assert_eq!(parse_line(b"N")?, Response::NotAvailable);
        assert_eq!(
            parse_line(b"PS 1")?,
            Response::Other {
                mnemonic: "PS".to_owned(),
                payload: "1".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn foreign_identities_and_bad_bytes_are_errors() {
        let d75 = parse_line(b"ID TH-D75");
        assert!(
            matches!(d75, Err(ProtocolError::UnexpectedIdentity { ref reply }) if reply == "TH-D75"),
            "{d75:?}"
        );
        let empty = parse_line(b"");
        assert!(
            matches!(empty, Err(ProtocolError::EmptyLine { .. })),
            "{empty:?}"
        );
        let non_ascii = parse_line(&[b'I', b'D', b' ', 0xFF]);
        assert!(
            matches!(non_ascii, Err(ProtocolError::NonAsciiLine { .. })),
            "{non_ascii:?}"
        );
        let bad_type = parse_line(b"TY ");
        assert!(
            matches!(
                bad_type,
                Err(ProtocolError::FieldParse { command: "TY", .. })
            ),
            "{bad_type:?}"
        );
    }
}
