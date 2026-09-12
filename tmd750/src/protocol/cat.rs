//! Qualified CAT identity, operating-mode, and gateway queries.
//!
//! Requests are the mnemonic plus a carriage return; replies are the
//! mnemonic, a space, and the payload, terminated the same way. A lone `?`
//! is a rejection and a lone `N` means the command is unavailable in the
//! current mode.

use crate::error::ProtocolError;
use crate::types::{
    Band, DvGatewayMode, FirmwareIdentity, OperatingMode, RadioModel, RadioType, SelectableMode,
};

/// Byte that terminates every CAT line.
pub const LINE_TERMINATOR: u8 = b'\r';

/// Typed CAT commands this crate sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `ID`: model string.
    Identify,
    /// `FV`: firmware identity.
    FirmwareVersion,
    /// `TY`: complete opaque radio-type payload.
    RadioType,
    /// `MD <band>`: read an operating mode.
    GetOperatingMode {
        /// Band to query.
        band: Band,
    },
    /// `MD <band>,<mode>`: select FM or DV.
    SetOperatingMode {
        /// Band to change.
        band: Band,
        /// Qualified CAT-selectable mode.
        mode: SelectableMode,
    },
    /// `GW`: read the persistent DV Gateway mode.
    GetGatewayMode,
}

impl Command {
    /// The two-letter mnemonic.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Identify => "ID",
            Self::FirmwareVersion => "FV",
            Self::RadioType => "TY",
            Self::GetOperatingMode { .. } | Self::SetOperatingMode { .. } => "MD",
            Self::GetGatewayMode => "GW",
        }
    }

    /// The bytes to send.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let body = match self {
            Self::Identify | Self::FirmwareVersion | Self::RadioType | Self::GetGatewayMode => {
                self.mnemonic().to_owned()
            }
            Self::GetOperatingMode { band } => format!("MD {}", u8::from(band)),
            Self::SetOperatingMode { band, mode } => {
                let mode = OperatingMode::from(mode);
                format!("MD {},{}", u8::from(band), u8::from(mode))
            }
        };
        let mut bytes = body.into_bytes();
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
    /// `TY <opaque printable payload>`.
    RadioType(RadioType),
    /// `MD <band>,<mode>`.
    OperatingMode {
        /// Band reported by the radio.
        band: Band,
        /// Current operating mode.
        mode: OperatingMode,
    },
    /// `GW <mode>`.
    GatewayMode(DvGatewayMode),
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
/// `TM-D750`, and [`ProtocolError::FieldParse`] for a malformed typed payload.
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
        "TY" => RadioType::new(payload)
            .map(Response::RadioType)
            .map_err(|error| ProtocolError::FieldParse {
                command: "TY",
                field: "radio_type",
                detail: error.to_string(),
            }),
        "MD" => parse_operating_mode(payload),
        "GW" => parse_decimal_u8(payload, "GW", "mode")
            .map(DvGatewayMode::from)
            .map(Response::GatewayMode),
        other => Ok(Response::Other {
            mnemonic: other.to_owned(),
            payload: payload.to_owned(),
        }),
    }
}

fn parse_operating_mode(payload: &str) -> Result<Response, ProtocolError> {
    let mut fields = payload.split(',');
    let band = fields.next().unwrap_or_default();
    let mode = fields.next().unwrap_or_default();
    if fields.next().is_some() || band.is_empty() || mode.is_empty() {
        return Err(ProtocolError::FieldParse {
            command: "MD",
            field: "payload",
            detail: "expected band,mode".to_owned(),
        });
    }
    let band = parse_decimal_u8(band, "MD", "band").and_then(|raw| {
        Band::try_from(raw).map_err(|error| ProtocolError::FieldParse {
            command: "MD",
            field: "band",
            detail: error.to_string(),
        })
    })?;
    let mode = OperatingMode::from(parse_decimal_u8(mode, "MD", "mode")?);
    Ok(Response::OperatingMode { band, mode })
}

fn parse_decimal_u8(
    value: &str,
    command: &'static str,
    field: &'static str,
) -> Result<u8, ProtocolError> {
    let parsed = value
        .parse::<u8>()
        .map_err(|error| ProtocolError::FieldParse {
            command,
            field,
            detail: error.to_string(),
        })?;
    if value == parsed.to_string() {
        Ok(parsed)
    } else {
        Err(ProtocolError::FieldParse {
            command,
            field,
            detail: format!("noncanonical decimal value {value:?}"),
        })
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
        assert_eq!(
            Command::GetOperatingMode { band: Band::A }.encode(),
            b"MD 0\r"
        );
        assert_eq!(
            Command::SetOperatingMode {
                band: Band::B,
                mode: SelectableMode::Dv,
            }
            .encode(),
            b"MD 1,1\r"
        );
        assert_eq!(Command::GetGatewayMode.encode(), b"GW\r");
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
            parse_line(b"TY K,2,1")?,
            Response::RadioType(RadioType::new("K,2,1")?)
        );
        assert_eq!(
            parse_line(b"MD 0,1")?,
            Response::OperatingMode {
                band: Band::A,
                mode: OperatingMode::Dv,
            }
        );
        assert_eq!(
            parse_line(b"GW 2")?,
            Response::GatewayMode(DvGatewayMode::Terminal),
            "the observed Terminal reply must parse as a named read value"
        );
        assert_eq!(
            parse_line(b"GW 1")?,
            Response::GatewayMode(DvGatewayMode::Unqualified(1)),
            "unobserved GW 1 must remain losslessly unqualified"
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

    #[test]
    fn malformed_mode_and_gateway_replies_are_errors() {
        for line in [
            b"MD 0".as_slice(),
            b"MD 2,1",
            b"MD 0,x",
            b"MD 00,1",
            b"MD +0,+1",
            b"GW x",
            b"GW 00",
        ] {
            let result = parse_line(line);
            assert!(
                matches!(result, Err(ProtocolError::FieldParse { .. })),
                "{line:?}: {result:?}"
            );
        }
        assert_eq!(
            parse_line(b"MD 0,7"),
            Ok(Response::OperatingMode {
                band: Band::A,
                mode: OperatingMode::Unqualified(7),
            })
        );
        assert_eq!(
            parse_line(b"GW 3"),
            Ok(Response::GatewayMode(DvGatewayMode::Unqualified(3)))
        );
    }
}
