//! A-prefixed CAT commands: APRS operations AS (packet data rate), BE (transmit
//! beacon), PT (beacon mode), MS (position source), and CS (APRS My Callsign),
//! plus the unrelated AE radio-identity query.
//!
//! Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;
use crate::types::radio_params::{BeaconMode, MyPositionSelection, PacketDataRate};
use crate::types::{AprsCallsign, ModelCode, SerialInformation, SerialNumber};

use super::Response;
use super::fields::{decimal_u8, empty_payload, split_exact};

/// Parse AE (serial info): `serial,model_code`.
///
/// Despite the AE mnemonic, this returns the radio serial number and model code.
/// Example: `C3C10368,K01`.
fn parse_ae(payload: &str) -> Result<Response, ProtocolError> {
    let [serial, model_code] = split_exact::<2>(payload, "AE")?;
    let serial_number = SerialNumber::new(serial).map_err(|error| ProtocolError::FieldParse {
        command: "AE".to_owned(),
        field: "serial_number".to_owned(),
        detail: error.to_string(),
    })?;
    let model_code = ModelCode::new(model_code).map_err(|error| ProtocolError::FieldParse {
        command: "AE".to_owned(),
        field: "model_code".to_owned(),
        detail: error.to_string(),
    })?;
    Ok(Response::SerialInformation(SerialInformation::new(
        serial_number,
        model_code,
    )))
}

/// Parse an APRS command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not an APRS command.
pub(crate) fn parse_aprs(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "AS" => Some(decimal_u8(payload, "AS", "data rate").and_then(|raw| {
            PacketDataRate::try_from(raw)
                .map(|data_rate| Response::PacketDataRate { data_rate })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "AS".into(),
                    field: "data rate".into(),
                    detail: e.to_string(),
                })
        })),
        "AE" => Some(parse_ae(payload)),
        "BE" => Some(empty_payload(payload, "BE").map(|()| Response::AprsBeaconTransmitAck)),
        "PT" => Some(decimal_u8(payload, "PT", "mode").and_then(|raw| {
            BeaconMode::try_from(raw)
                .map(|mode| Response::BeaconMode { mode })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "PT".into(),
                    field: "mode".into(),
                    detail: e.to_string(),
                })
        })),
        "MS" => Some(decimal_u8(payload, "MS", "selection").and_then(|raw| {
            MyPositionSelection::try_from(raw)
                .map(|selection| Response::MyPositionSelection { selection })
                .map_err(|error| ProtocolError::FieldParse {
                    command: "MS".to_owned(),
                    field: "selection".to_owned(),
                    detail: error.to_string(),
                })
        })),
        "CS" => Some(if payload.is_empty() {
            Ok(Response::AprsCallsign { callsign: None })
        } else {
            AprsCallsign::new(payload)
                .map(Some)
                .map(|callsign| Response::AprsCallsign { callsign })
                .map_err(|error| ProtocolError::FieldParse {
                    command: "CS".to_owned(),
                    field: "callsign".to_owned(),
                    detail: error.to_string(),
                })
        }),
        _ => None,
    }
}
