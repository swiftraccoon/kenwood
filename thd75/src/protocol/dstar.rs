//! D-STAR (Digital Smart Technologies for Amateur Radio) commands: DS and GW.
//!
//! Provides parsing of responses for the D-STAR-related CAT protocol
//! commands. Serialization is handled inline by the main dispatcher.

use crate::error::ProtocolError;
use crate::types::radio_params::{DstarSlot, DvGatewayMode};

use super::Response;
use super::fields::decimal_u8;

/// Parse a D-STAR command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a D-STAR command.
pub(crate) fn parse_dstar(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "DS" => Some(decimal_u8(payload, "DS", "slot").and_then(|raw| {
            DstarSlot::try_from(raw)
                .map(|slot| Response::DstarSlot { slot })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "DS".into(),
                    field: "slot".into(),
                    detail: e.to_string(),
                })
        })),
        "GW" => Some(decimal_u8(payload, "GW", "value").and_then(|raw| {
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
