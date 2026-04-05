//! D-STAR (Digital Smart Technologies for Amateur Radio) commands: DS, CS, GW.
//!
//! Provides parsing of responses for the 3 D-STAR-related CAT protocol
//! commands. Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;
use crate::types::radio_params::{CallsignSlot, DstarSlot, DvGatewayMode};

use super::Response;

/// Parse a `u8` from a string field.
fn parse_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    s.parse::<u8>().map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid u8: {s:?}"),
    })
}

/// Parse a D-STAR command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a D-STAR command.
pub(crate) fn parse_dstar(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "DS" => Some(parse_u8_field(payload, "DS", "slot").and_then(|raw| {
            DstarSlot::try_from(raw)
                .map(|slot| Response::DstarSlot { slot })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "DS".into(),
                    field: "slot".into(),
                    detail: e.to_string(),
                })
        })),
        "CS" => Some(parse_u8_field(payload, "CS", "slot").and_then(|raw| {
            CallsignSlot::try_from(raw)
                .map(|slot| Response::ActiveCallsignSlot { slot })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "CS".into(),
                    field: "slot".into(),
                    detail: e.to_string(),
                })
        })),
        "GW" => Some(parse_u8_field(payload, "GW", "value").and_then(|raw| {
            DvGatewayMode::try_from(raw)
                .map(|value| Response::Gateway { value })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "GW".into(),
                    field: "value".into(),
                    detail: e.to_string(),
                })
        })),
        _ => None,
    }
}
