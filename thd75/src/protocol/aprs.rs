//! APRS-related commands: AS (TNC baud), AE (serial info), PT (beacon type),
//! MS (position source), and CS (APRS My Callsign).
//!
//! Provides parsing of responses for the APRS-related CAT protocol
//! commands. Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;
use crate::types::AprsCallsign;
use crate::types::radio_params::{BeaconMode, MyPositionSelection, TncBaud};

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
fn parse_ae(payload: &str) -> Result<Response, ProtocolError> {
    let (serial, model_code) =
        payload
            .split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "AE".to_owned(),
                field: "all".to_owned(),
                detail: format!(
                    "expected 8-character serial and 3-character model, got {payload:?}"
                ),
            })?;
    if serial.len() != 8 || !serial.is_ascii() {
        return Err(ProtocolError::FieldParse {
            command: "AE".to_owned(),
            field: "serial".to_owned(),
            detail: format!("expected exactly 8 ASCII characters, got {serial:?}"),
        });
    }
    if model_code.len() != 3 || !model_code.is_ascii() {
        return Err(ProtocolError::FieldParse {
            command: "AE".to_owned(),
            field: "model_code".to_owned(),
            detail: format!("expected exactly 3 ASCII characters, got {model_code:?}"),
        });
    }
    Ok(Response::SerialInfo {
        serial: serial.to_owned(),
        model_code: model_code.to_owned(),
    })
}

/// Parse an APRS command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not an APRS command.
pub(crate) fn parse_aprs(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "AS" => Some(parse_u8_field(payload, "AS", "rate").and_then(|raw| {
            TncBaud::try_from(raw)
                .map(|rate| Response::TncBaud { rate })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "AS".into(),
                    field: "rate".into(),
                    detail: e.to_string(),
                })
        })),
        "AE" => Some(parse_ae(payload)),
        "PT" => Some(parse_u8_field(payload, "PT", "mode").and_then(|raw| {
            BeaconMode::try_from(raw)
                .map(|mode| Response::BeaconType { mode })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "PT".into(),
                    field: "mode".into(),
                    detail: e.to_string(),
                })
        })),
        "MS" => Some(parse_u8_field(payload, "MS", "selection").and_then(|raw| {
            MyPositionSelection::try_from(raw)
                .map(|selection| Response::MyPositionSelection { selection })
                .map_err(|error| ProtocolError::FieldParse {
                    command: "MS".to_owned(),
                    field: "selection".to_owned(),
                    detail: error.to_string(),
                })
        })),
        "CS" => Some(
            AprsCallsign::new(payload)
                .ok_or_else(|| ProtocolError::FieldParse {
                    command: "CS".to_owned(),
                    field: "callsign".to_owned(),
                    detail: format!(
                        "expected at most {} non-control ASCII bytes, got {payload:?}",
                        AprsCallsign::MAX_LEN
                    ),
                })
                .map(|callsign| Response::AprsCallsign { callsign }),
        ),
        _ => None,
    }
}
