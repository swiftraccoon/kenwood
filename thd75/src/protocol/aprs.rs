//! APRS-related commands: AS (TNC baud), AE (serial info), PT (beacon type), MS (position source).
//!
//! Provides parsing of responses for the 4 APRS-related CAT protocol
//! commands. Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;

use super::Response;

/// Parse a `u8` from a string field.
fn parse_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    s.parse::<u8>().map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid u8: {s:?}"),
    })
}

/// Parse AE (serial info): `serial,model_code`.
///
/// Despite the AE mnemonic, this returns the radio serial number and model code.
/// Example: `C3C10368,K01`.
fn parse_ae(payload: &str) -> Response {
    if let Some((serial, model_code)) = payload.split_once(',') {
        Response::SerialInfo {
            serial: serial.to_owned(),
            model_code: model_code.to_owned(),
        }
    } else {
        // Fallback: treat whole payload as serial with empty model_code
        Response::SerialInfo {
            serial: payload.to_owned(),
            model_code: String::new(),
        }
    }
}

/// Parse an APRS command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not an APRS command.
pub(crate) fn parse_aprs(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "AS" => Some(parse_u8_field(payload, "AS", "rate").map(|rate| Response::TncBaud { rate })),
        "AE" => Some(Ok(parse_ae(payload))),
        "PT" => Some(parse_u8_field(payload, "PT", "mode").map(|mode| Response::BeaconType { mode })),
        "MS" => Some(parse_u8_field(payload, "MS", "source").map(|source| Response::PositionSource { source })),
        _ => None,
    }
}
